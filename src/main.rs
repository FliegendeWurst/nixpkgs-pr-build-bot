#![feature(round_char_boundary, iter_map_windows)]

use std::{
	collections::{HashMap, HashSet},
	env,
	error::Error,
	io::Cursor,
	num::NonZero,
	process::Stdio,
	sync::Arc,
	time::Duration,
};

use anyhow::Context;
use brace_expand::brace_expand;
use bytes::Bytes;
use frankenstein::{
	AllowedUpdate, AsyncApi, AsyncTelegramApi, Chat, EditMessageTextParams, GetUpdatesParams, InlineKeyboardButton,
	InlineKeyboardMarkup, LinkPreviewOptions, MaybeInaccessibleMessage, Message, ReplyMarkup, ReplyParameters,
	SendMessageParams, UpdateContent,
};
use once_cell::sync::Lazy;
use sysinfo::Disks;
use tokio::{
	io::AsyncWriteExt,
	process::Command,
	sync::{Mutex, Semaphore},
	task,
};
use zip::ZipArchive;

static TASKS_RUNNING: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(TASKS.get()));
static PRS_RUNNING: Lazy<Mutex<HashSet<u32>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static PRS_BUILDING: Lazy<Mutex<HashMap<u32, Vec<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static PRS_OUTPATHS: Lazy<Mutex<HashMap<u32, Vec<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static GIT_OPERATIONS: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

static NIX_USER: Lazy<String> = Lazy::new(|| env::var("NIXPKGS_PR_BUILD_BOT_NIX_USER").unwrap_or("nobody".to_owned()));
static SUDO_PASSWORD: Lazy<String> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_SUDO_PASSWORD").expect(
		"NIXPKGS_PR_BUILD_BOT_SUDO_PASSWORD not set to password required to sudo as $NIXPKGS_PR_BUILD_BOT_NIX_USER (nobody)",
	)
});
static NIXPKGS_DIRECTORY: Lazy<String> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_NIXPKGS_DIRECTORY")
		.expect("NIXPKGS_PR_BUILD_BOT_NIXPKGS_DIRECTORY not set to jj-colocated checkout of nixpkgs")
});
static CORES: Lazy<Option<u64>> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_CORES")
		.ok()
		.map(|x| x.parse::<u64>().ok())
		.flatten()
		.map(|x| NonZero::new(x))
		.flatten()
		.map(|x| x.get())
});
static JOBS: Lazy<NonZero<u64>> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_JOBS")
		.ok()
		.map(|x| x.parse::<u64>().ok())
		.flatten()
		.map(|x| NonZero::new(x))
		.flatten()
		.unwrap_or(NonZero::new(6).unwrap())
});
static TASKS: Lazy<NonZero<usize>> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_TASKS")
		.ok()
		.map(|x| x.parse::<usize>().ok())
		.flatten()
		.map(|x| NonZero::new(x))
		.flatten()
		.unwrap_or(NonZero::new(10).unwrap())
});
static WASTEBIN_URL: Lazy<String> =
	Lazy::new(|| env::var("NIXPKGS_PR_BUILD_BOT_WASTEBIN").unwrap_or("https://paste.fliegendewurst.eu".to_owned()));
static CONTACT: Lazy<String> =
	Lazy::new(|| env::var("NIXPKGS_PR_BUILD_BOT_CONTACT").unwrap_or("this bot's owner".to_owned()));
static DESCRIPTION: Lazy<String> = Lazy::new(|| {
	env::var("NIXPKGS_PR_BUILD_BOT_DESCRIPTION").unwrap_or("the nixpkgs revision currently checked out".to_owned())
});
static GH_TOKEN: Lazy<Option<String>> = Lazy::new(|| env::var("NIXPKGS_PR_BUILD_BOT_GH_TOKEN").ok());

#[tokio::main]
async fn main() {
	loop {
		if let Err(e) = real_main().await {
			eprintln!("general error: {e:?}");
		}
		tokio::time::sleep(Duration::from_secs(10)).await;
	}
}

trait MessageGetChat {
	fn chat(&self) -> &Chat;
}

impl MessageGetChat for MaybeInaccessibleMessage {
	fn chat(&self) -> &Chat {
		match self {
			MaybeInaccessibleMessage::Message(message) => &message.chat,
			MaybeInaccessibleMessage::InaccessibleMessage(inaccessible_message) => &inaccessible_message.chat,
		}
	}
}

async fn real_main() -> Result<(), Box<dyn Error>> {
	let disks = Disks::new_with_refreshed_list();
	let biggest_disk = disks
		.iter()
		.max_by_key(|x| x.total_space())
		.expect("failed to detect /nix/store disk");

	let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
	Lazy::force(&NIX_USER);
	Lazy::force(&SUDO_PASSWORD);
	Lazy::force(&NIXPKGS_DIRECTORY);
	Lazy::force(&CORES);
	Lazy::force(&JOBS);
	Lazy::force(&TASKS);
	Lazy::force(&WASTEBIN_URL);
	Lazy::force(&CONTACT);
	Lazy::force(&GH_TOKEN);
	let api = AsyncApi::new(&token);
	let api = Arc::new(api);

	macro_rules! reply {
		($msg:expr, $txt:expr) => {
			api.send_message(
				&SendMessageParams::builder()
					.chat_id($msg.chat.id)
					.link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
					.text($txt)
					.build(),
			)
			.await?;
		};
	}

	// Fetch new updates via long poll method
	let mut update_params = GetUpdatesParams::builder()
		.allowed_updates(vec![
			AllowedUpdate::Message,
			AllowedUpdate::MessageReaction,
			AllowedUpdate::CallbackQuery,
		])
		.build();
	let mut messages = HashMap::new();
	loop {
		let stream = api.get_updates(&update_params).await;
		if let Err(e) = stream {
			eprintln!("telegram error: {e:?}");
			tokio::time::sleep(Duration::from_secs(10)).await;
			continue;
		}
		let stream = stream.unwrap();
		'update: for update in stream.result {
			update_params.offset = Some(i64::from(update.update_id) + 1);

			// Handle inline keyboard presses.
			if let UpdateContent::CallbackQuery(cb) = &update.content {
				let Some(id) = cb.message.as_ref().map(|x| x.chat().id) else {
					continue;
				};
				if let Some(num) = cb.data.as_ref().map(|x| x.parse().ok()).flatten() {
					let out_paths = PRS_OUTPATHS.lock().await;
					let Some(out_paths) = out_paths.get(&num) else {
						continue;
					};
					if out_paths.len() <= 10 {
						// send directly
						let reply = format!("📝 Out paths for PR {num}\n{}", out_paths.join("\n"));
						api.send_message(
							&SendMessageParams::builder()
								.chat_id(id)
								.link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
								.text(reply)
								.build(),
						)
						.await?;
					} else {
						let reply = format!(
							"📝 Out paths for PR {num}\n👉 Full list: {}",
							paste("PR #{num} - out paths", "", &out_paths.join("\n")).await?
						);
						api.send_message(
							&SendMessageParams::builder()
								.chat_id(id)
								.link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
								.text(reply)
								.build(),
						)
						.await?;
					}
				}
				continue;
			}

			// If the received update contains a new message...
			let mut message = if let UpdateContent::MessageReaction(reaction) = &update.content {
				// TODO: use api.get_messages for older messages
				messages.get(&reaction.message_id).cloned()
			} else {
				None
			};
			if let UpdateContent::Message(m) = update.content {
				message = Some(m);
			}
			let Some(message) = message else { continue };
			messages.insert(message.message_id, message.clone());
			let Some(data) = &message.text else {
				continue;
			};
			for data in data.split('\n') {
				let (mut command, mut args) = data
					.split_once(' ')
					.map(|x| (x.0.to_owned(), x.1.to_owned()))
					.unwrap_or((data.to_owned(), "".to_owned()));

				if let Some(pr_num) = command.strip_prefix("https://github.com/NixOS/nixpkgs/pull/") {
					let pr_num = pr_num.split(|x: char| !x.is_digit(10)).next().unwrap();
					args = format!("{pr_num} {args}");
					command = "/pr".to_owned();
				}

				let args = args.to_owned();

				// Print received text message to stdout.
				println!(
					"<{} {:?}>: {}",
					message.from.as_ref().map(|x| x.id).unwrap_or(0),
					&message.from.as_ref().map(|x| x.first_name.clone()).unwrap_or_default(),
					data
				);

				let full = command.to_ascii_lowercase() == "/full";
				if full {
					command = "/pr".to_owned();
				}

				match &*command.to_ascii_lowercase() {
					"/start" => {
						reply!(message, format!("This bot allows you to build nixpkgs PRs.\nUsage: /pr <number> <packages> -<exclude packages>
You can also just send the PR as URL. Packages is a space seperated list of additional packages to build. You can exclude certain packages by prefixing them with -.
PRs are tested by cherry-picking their commits on {}.
Ping {} if you have trouble.", *DESCRIPTION, *CONTACT));
					},
					"/pr" => {
						if biggest_disk.available_space() < 200_000_000_000 {
							reply!(message, "🫠 Internal error: out of disk space");
							continue 'update;
						}
						let api = Arc::clone(&api);
						let message = message.clone();
						task::spawn(async move {
							let id = message.chat.id;
							let (num, pkgs) = if args.contains(' ') {
								let (num, pkgs) = args.split_once(' ').unwrap();
								(
									num.parse().unwrap_or(0),
									pkgs.split_whitespace().map(|x| x.to_owned()).collect(),
								)
							} else {
								(args.parse().unwrap_or(0), vec![])
							};
							if num == 0 {
								return;
							}
							if let Err(e) = process_pr(Arc::clone(&api), message, num, pkgs, full).await {
								println!("error: {:?}", e);
								let _ = api
									.send_message(
										&SendMessageParams::builder()
											.chat_id(id)
											.text(format!("🤯 Internal error: {e:?}"))
											.build(),
									)
									.await;
							}
							let mut prs = PRS_RUNNING.lock().await;
							prs.remove(&num);
							drop(prs);
							let mut prs_building = PRS_BUILDING.lock().await;
							prs_building.remove(&num);
							drop(prs_building);
						});
					},
					"/status" => {
						let prs_building = PRS_BUILDING.lock().await;
						let mut status = String::new();
						for (num, pkgs) in prs_building.iter() {
							if !status.is_empty() {
								status.push('\n');
							}
							let pkgs = if pkgs.len() <= 10 {
								pkgs.join(" ")
							} else {
								format!("{} packages", pkgs.len())
							};
							status += &format!("PR {num}: {}", pkgs);
						}
						if !status.is_empty() {
							reply!(message, status);
						}
						drop(prs_building);
					},
					_ => {
						reply!(message, format!("error: unknown command"));
					},
				}
			}
		}
	}
}

async fn process_pr(
	api: Arc<AsyncApi>,
	msg: Message,
	num: u32,
	mut pkgs: Vec<String>,
	full: bool,
) -> Result<(), anyhow::Error> {
	macro_rules! reply {
		($msg:expr, $txt:expr) => {
			api.send_message(
				&SendMessageParams::builder()
					.chat_id($msg.chat.id)
					.reply_parameters(
						ReplyParameters::builder()
							.message_id($msg.message_id)
							.chat_id($msg.chat.id)
							.build(),
					)
					.text($txt)
					.build(),
			)
			.await
			.context("sending reply")?
			.result
		};
		($msg:expr, $txt:expr, $reply_markup:expr) => {
			api.send_message(
				&SendMessageParams::builder()
					.chat_id($msg.chat.id)
					.reply_parameters(
						ReplyParameters::builder()
							.message_id($msg.message_id)
							.chat_id($msg.chat.id)
							.build(),
					)
					.reply_markup($reply_markup)
					.text($txt)
					.build(),
			)
			.await
			.context("sending reply")?
			.result
		};
	}
	// first verify the PR is not against a stable branch
	let json = api
		.client
		.execute(
			api.client
				.get(format!("https://api.github.com/repos/NixOS/nixpkgs/pulls/{num}"))
				.header(
					"User-Agent",
					concat!("nixpkgs-pr-build-bot/", env!("CARGO_PKG_VERSION")),
				)
				.build()?,
		)
		.await
		.context("getting PR base ref")?;
	let json: serde_json::Value = json.json().await.context("decoding GH API response")?;
	let base = json
		.pointer("/base/ref")
		.map(|x| x.as_str())
		.flatten()
		.unwrap_or("release-");

	if base != "master" && base != "staging" && base != "staging-next" {
		reply!(msg, "🛑 PR does not target master/staging/staging-next");
		return Ok(());
	}

	let Ok(ticket) = TASKS_RUNNING.try_acquire() else {
		reply!(msg, "🤖 too many PRs already running");
		return Ok(());
	};
	let mut prs = PRS_RUNNING.lock().await;
	if prs.contains(&num) {
		reply!(msg, "🤖 already processing that PR");
		return Ok(());
	}
	prs.insert(num);
	drop(prs);
	let mut msg_text = format!("⏳ PR {num}, fetching ...");
	let my_msg = reply!(msg, &msg_text);
	macro_rules! extend_message {
		($new_text:expr) => {
			msg_text.push('\n');
			msg_text += &($new_text);
			api.edit_message_text(
				&EditMessageTextParams::builder()
					.chat_id(msg.chat.id)
					.message_id(my_msg.message_id)
					.text(&msg_text)
					.build(),
			)
			.await
			.context("editing response")?;
		};
	}

	'eval: {
		// for reference: https://github.com/Mic92/nixpkgs-review/pull/435
		if !full {
			break 'eval;
		}
		let Some(token) = GH_TOKEN.as_ref() else {
			extend_message!("⚠️ no GH token configured");
			break 'eval;
		};
		let Some(commit_sha) = json.pointer("/head/sha").map(|x| x.as_str()).flatten() else {
			extend_message!("⚠️ could not fetch PR HEAD");
			break 'eval;
		};
		let workflow_runs_resp: serde_json::Value = api
			.client
			.execute(
				api.client
					.get(format!(
						"https://api.github.com/repos/NixOS/nixpkgs/actions/runs?head_sha={commit_sha}"
					))
					.header(
						"User-Agent",
						concat!("nixpkgs-pr-build-bot/", env!("CARGO_PKG_VERSION")),
					)
					.build()?,
			)
			.await
			.context("getting GHA runs for PR")?
			.json()
			.await
			.context("decoding GH api response for runs")?;

		let Some(runs) = workflow_runs_resp
			.pointer("/workflow_runs")
			.map(|x| x.as_array())
			.flatten()
		else {
			extend_message!("⚠️ no workflow runs found");
			break 'eval;
		};

		let Some(run) = runs
			.into_iter()
			.filter(|x| x.pointer("/name").map(|x| x.as_str()).flatten() == Some("Eval"))
			.next()
		else {
			extend_message!("⚠️ no eval run found");
			break 'eval;
		};

		let Some(artifacts_url) = run.pointer("/artifacts_url").map(|x| x.as_str()).flatten() else {
			extend_message!("⚠️ no eval artifact found (wait for eval to complete)");
			break 'eval;
		};

		let artifact: serde_json::Value = api
			.client
			.execute(
				api.client
					.get(artifacts_url)
					.header(
						"User-Agent",
						concat!("nixpkgs-pr-build-bot/", env!("CARGO_PKG_VERSION")),
					)
					.build()?,
			)
			.await
			.context("getting GHA Eval artifact for PR")?
			.json()
			.await
			.context("decoding GH api response for Eval artifact")?;
		let Some(artifacts) = artifact
			.pointer("/artifacts")
			.map(|x| {
				x.as_array()
					.map(|x| x.into_iter().flat_map(|x| x.as_object()).collect::<Vec<_>>())
			})
			.flatten()
		else {
			extend_message!("⚠️ eval artifact not found");
			break 'eval;
		};
		let Some(artifact_id) = artifacts
			.iter()
			.filter(|x| x.get("name").map(|x| x.as_str()).flatten() == Some("comparison"))
			.map(|x| x.get("id").map(|x| x.as_u64()).flatten())
			.next()
			.flatten()
		else {
			extend_message!("⚠️ artifact ID not found");
			break 'eval;
		};
		let zip_url = format!("https://api.github.com/repos/NixOS/nixpkgs/actions/artifacts/{artifact_id}/zip");
		let artifact: Bytes = api
			.client
			.execute(
				api.client
					.get(zip_url)
					.bearer_auth(token)
					.header(
						"User-Agent",
						concat!("nixpkgs-pr-build-bot/", env!("CARGO_PKG_VERSION")),
					)
					.build()?,
			)
			.await
			.context("downloading GHA Eval artifact for PR")?
			.bytes()
			.await
			.context("getting bytes of GHA Eval artifact")?;

		let mut zip = ZipArchive::new(Cursor::new(&artifact)).context("reading GHA Eval zip")?;

		let changed_paths = zip
			.by_name("changed-paths.json")
			.context("finding changed-paths json")?;

		let changed_paths: serde_json::Value =
			serde_json::from_reader(changed_paths).context("unzipping changed-paths json")?;

		println!("{changed_paths:?}");

		let Some(added) = changed_paths.pointer("/attrdiff/added").map(|x| x.as_array()).flatten() else {
			extend_message!("⚠️ added packages not found");
			break 'eval;
		};
		let Some(changed) = changed_paths
			.pointer("/attrdiff/changed")
			.map(|x| x.as_array())
			.flatten()
		else {
			extend_message!("⚠️ changed packages not found");
			break 'eval;
		};
		for x in added.into_iter().chain(changed.into_iter()).flat_map(|x| x.as_str()) {
			if x == "nixos-install-tools" || x == "tests.nixos-functions.nixos-test" {
				continue; // stupidly always get rebuilt
			}
			pkgs.push(x.to_owned());
		}
	}

	pkgs = pkgs
		.into_iter()
		.map::<Box<dyn Iterator<Item = _>>, _>(|x| {
			if x.contains("override") {
				Box::new(Some(x).into_iter())
			} else {
				Box::new(brace_expand(&x).into_iter())
			}
		})
		.flatten()
		.collect();

	let git_operations = GIT_OPERATIONS.lock().await;
	let rev = String::from_utf8(
		Command::new("jj")
			.current_dir(&*NIXPKGS_DIRECTORY)
			.args("log --limit 1 --no-graph -T commit_id -r @".split_whitespace())
			.stdout(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await
			.context("running jj to get current working copy")?
			.stdout,
	)
	.unwrap();
	let new_commit = String::from_utf8(
		Command::new("jj")
			.current_dir(&*NIXPKGS_DIRECTORY)
			.args("new --message root --no-edit root()".split_whitespace())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await
			.context("running jj to create new commit")?
			.stderr, // why is this printed to stderr ???
	)
	.unwrap();
	let new_commit = new_commit
		.split(' ')
		.skip(4)
		.next()
		.context("failed to create new commit using jj")?;

	let tmp = format!("/tmp/nixpkgs-{num}");
	Command::new("git")
		.current_dir(&*NIXPKGS_DIRECTORY)
		.args(["worktree", "add", &tmp, new_commit])
		.spawn()
		.context("creating worktree with git")?
		.wait()
		.await
		.context("creating worktree with git")?;

	Command::new("git")
		.current_dir(&tmp)
		.args(["fetch", "origin", "master", "staging", "staging-next"])
		.spawn()
		.context("fetching upstream with git")?
		.wait()
		.await
		.context("fetching upstream with git")?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["fetch", "origin", &format!("pull/{num}/head")])
		.spawn()
		.context("fetching PR HEAD with git")?
		.wait()
		.await
		.context("fetching PR HEAD with git")?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["switch", "-C", &format!("nixpkgs-{num}")])
		.spawn()
		.context("switching to new branch with git")?
		.wait()
		.await
		.context("switching to new branch with git")?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["restore", "-s", &rev, "--", "."])
		.spawn()
		.context("restoring working copy with git")?
		.wait()
		.await
		.context("restoring working copy with git")?;
	Command::new("git")
		.current_dir(&tmp)
		.args("add .".split(' '))
		.spawn()
		.context("adding working copy with git")?
		.wait()
		.await
		.context("restoring working copy with git")?;
	Command::new("git")
		.current_dir(&tmp)
		.args("commit -a --message wip".split(' '))
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
		.context("committing working copy with git")?
		.wait()
		.await
		.context("committing working copy with git")?;
	let output = Command::new("git")
		.current_dir(&tmp)
		.args("log --oneline FETCH_HEAD --not origin/master origin/staging origin/staging-next".split(' '))
		.stdout(Stdio::piped())
		.spawn()
		.context("getting new commits in PR with git")?
		.wait_with_output()
		.await
		.context("getting new commits in PR with git")?;
	let out = String::from_utf8(output.stdout)?;
	let mut lines = out.split('\n').filter(|x| !x.is_empty()).collect::<Vec<_>>();
	lines.reverse();
	println!("PR {num}, changes:\n{}", lines.join("\n"));
	extend_message!(format!("⏳ PR {num}, changes:\n{}", lines.join("\n")));
	let mut doit = true;
	let mut warn_merge = false;
	for line in &lines {
		let id = line.split_once(' ').unwrap().0;
		let cp = Command::new("git")
			.current_dir(&tmp)
			.args(format!("cherry-pick --allow-empty -x {id}").split(' '))
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.context("cherry-picking PR commit with git")?
			.wait_with_output()
			.await
			.context("cherry-picking PR commit with git")?;
		let output = String::from_utf8_lossy(&cp.stdout);
		let output_err = String::from_utf8_lossy(&cp.stderr);
		if !cp.status.success() && !output_err.contains("--allow-empty") {
			if output_err.contains("is a merge") {
				warn_merge = true;
				continue;
			}
			let diff = Command::new("git")
				.current_dir(&tmp)
				.args(&["diff"])
				.stdout(Stdio::piped())
				.spawn()
				.context("getting conflict diff with git")?
				.wait_with_output()
				.await
				.context("getting conflict diff with git")?;
			let output_diff = String::from_utf8_lossy(&diff.stdout);
			let url = paste(&format!("PR {num} - cherry-pick conflict"), "", &format!(
				"git cherry-pick standard output:\n{output}\ngit cherry-pick standard error:\n{output_err}\ngit diff output:\n{output_diff}"
			))
			.await.context("uploading diff output as paste")?;
			reply!(
				msg,
				format!("😢 PR {num}, cherry-pick of {id} failed\n👉 Conflict: {url}")
			);
			doit = false;
			break;
		}
	}
	drop(git_operations);

	for line in &lines {
		if line.is_empty() {
			continue;
		}
		let Some((pkg, _)) = line.split_once(' ').map(|x| x.1.split_once(": ")).flatten() else {
			continue;
		};
		if pkg.starts_with("Revert")
			|| pkg.starts_with("Merge")
			|| pkg.starts_with("treewide")
			|| pkg.starts_with("nixos")
			|| pkg.starts_with("lib.")
			|| pkg.starts_with('-')
			|| pkg.contains(' ')
			|| pkg.starts_with("maintainers")
		{
			continue;
		}
		if pkg.contains('{') {
			for pkg in brace_expand::brace_expand(pkg) {
				pkgs.push(pkg);
			}
		} else {
			pkgs.push(pkg.to_owned());
		}
	}
	pkgs.sort();
	pkgs.dedup();
	let to_remove = pkgs
		.iter()
		.flat_map(|x| x.strip_prefix('-'))
		.map(|x| x.to_owned())
		.collect::<Vec<_>>();
	for pkg in to_remove {
		pkgs.remove_item(pkg);
	}
	pkgs.retain(|x| !x.starts_with('-'));

	let pkgs_to_build = pkgs.join(" ");
	let mut prs_building = PRS_BUILDING.lock().await;
	prs_building.insert(num, pkgs.clone());
	drop(prs_building);
	println!("PR {num}, building: {pkgs_to_build}");
	let pkgs_to_build = if pkgs_to_build.len() > 1000 {
		format!("{} packages", pkgs.len())
	} else {
		pkgs_to_build
	};
	if doit {
		extend_message!(format!("⏳ PR {num}, building: {pkgs_to_build}"));

		let mut nix_args = vec![
			"-S".to_owned(),
			"-u".to_owned(),
			NIX_USER.clone(),
			"--preserve-env=NIXPKGS_ALLOW_UNFREE,NIXPKGS_ALLOW_INSECURE".to_owned(),
			"nix-shell".to_owned(),
			"--run".to_owned(),
			"sh -c 'echo $buildInputs'".to_owned(),
			"-k".to_owned(),
			"-j".to_owned(),
			format!("{}", *JOBS),
			"-I".to_owned(),
			format!("nixpkgs={tmp}"),
		];
		if let Some(cores) = *CORES {
			nix_args.extend(format!("--cores {cores}").split(' ').map(|x| x.to_owned()));
		}
		nix_args.push("-p".to_owned());
		for x in pkgs {
			if x.starts_with("pkgs") {
				nix_args.push(x);
			} else {
				nix_args.push(
					format!(
						r#"if (lib.hasAttrByPath (lib.splitString "." "{x}.meta") pkgs)
						then (
						 	if (
								lib.meta.availableOn stdenv.buildPlatform pkgs.{x}
								&& !pkgs.{x}.meta.broken
								&& (builtins.tryEval pkgs.{x}.outPath).success
							)
							then pkgs.{x}
							else null
						) else undefined-variable.override {{ pname = "{x}"; }}"#
					)
					.replace('\n', "")
					.replace('\t', " ")
					.replace("     ", " ")
					.replace("    ", " ")
					.replace("   ", " ")
					.replace("  ", " "),
				);
			}
		}
		let mut nix_output = Command::new("sudo")
			.current_dir(&tmp)
			.env("NIXPKGS_ALLOW_UNFREE", "1")
			.env("NIXPKGS_ALLOW_INSECURE", "1")
			.args(nix_args)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.context("running nix")?;
		nix_output
			.stdin
			.as_mut()
			.unwrap()
			.write_all(format!("{}\n", &*SUDO_PASSWORD).as_bytes())
			.await
			.context("running nix")?;
		let nix_output = nix_output.wait_with_output().await?;
		if nix_output.status.success() {
			let stdout = String::from_utf8_lossy(&nix_output.stdout);
			let built: Vec<&str> = stdout.split(' ').collect();
			let mut out_paths = PRS_OUTPATHS.lock().await;
			out_paths.insert(num, built.iter().map(|&x| x.to_owned()).collect());
			drop(out_paths);
			let built: Vec<_> = built
				.into_iter()
				// strip nix-store prefix
				.map(|x| if x.len() > 44 { &x[44..] } else { x })
				.map(|x| x.trim_ascii_end())
				.collect();
			let mut built_for_real = built
				.iter()
				.filter(|x| !x.starts_with("undefined-variable"))
				.copied()
				.collect::<Vec<_>>()
				.join(" ");
			if built_for_real.len() > 1000 {
				built_for_real = format!(
					"{} packages",
					built.iter().filter(|x| !x.starts_with("undefined-variable")).count()
				);
			}
			let warn_undefined = built
				.iter()
				.flat_map(|x| x.strip_prefix("undefined-variable-"))
				.collect::<Vec<_>>()
				.join(" ");
			let mut extra = String::new();
			if warn_merge {
				extra += "\n⚠️ PR contains merge commits";
			}
			if !warn_undefined.is_empty() {
				extra += &format!("\n⚠️ Undefined variables: {warn_undefined}");
			}
			if built_for_real.is_empty() {
				reply!(msg, format!("❓ PR {num}, nothing built{extra}"));
			} else {
				let out_paths = InlineKeyboardButton::builder()
					.text("📝 out paths")
					.callback_data(format!("{num}"))
					.build();
				let open_pr = InlineKeyboardButton::builder()
					.text("🌐 open PR")
					.url(format!("https://github.com/NixOS/nixpkgs/pull/{num}"))
					.build();
				let keyboard = vec![vec![out_paths, open_pr]];
				let inline = InlineKeyboardMarkup::builder().inline_keyboard(keyboard).build();
				let markup = ReplyMarkup::InlineKeyboardMarkup(inline);
				reply!(msg, format!("✅ PR {num}, built {built_for_real}{extra}"), markup);
			}
		} else {
			let stripped = strip_ansi_escapes::strip(&nix_output.stderr);
			let stdout = String::from_utf8_lossy(&stripped);
			if stdout.split('\n').skip(10).next().is_none() {
				reply!(msg, format!("💥 PR {num}, build failed"));
				reply!(msg, stdout);
			} else {
				let text = stdout.as_ref();
				let url = paste(&format!("PR #{num} - summary"), &format!("PR #{num}"), text)
					.await
					.context("uploading logs as paste")?;
				reply!(msg, format!("💥 PR {num}, build failed\n👉 Full log: {url}",));
			}
		}
	}

	let git_operations = GIT_OPERATIONS.lock().await;
	Command::new("git")
		.current_dir(&*NIXPKGS_DIRECTORY)
		.args(["worktree", "remove", "--force", &tmp])
		.spawn()
		.context("removing worktree with git")?
		.wait()
		.await
		.context("removing worktree with git")?;
	//Command::new("git")
	//	.current_dir(&*NIXPKGS_DIRECTORY)
	//	.args(["branch", "-D", &format!("nixpkgs-{num}")])
	//	.spawn()?
	//	.wait()
	//	.await?;
	drop(git_operations);

	drop(ticket);

	Ok(())
}

pub trait VecRemoveItem<T, U> {
	fn remove_item(&mut self, item: U) -> Option<T>
	where
		T: PartialEq<U>;
}

impl<T: PartialEq<U>, U> VecRemoveItem<T, U> for Vec<T> {
	fn remove_item(&mut self, item: U) -> Option<T> {
		self.iter().position(|n| n == &item).map(|idx| self.remove(idx))
	}
}

async fn paste(title: &str, title_prefix: &str, mut text: &str) -> Result<String, anyhow::Error> {
	let mut map = serde_json::json!({
		"title": title,
		"extension": "log",
		"expires": 7 * 24 * 60 * 60
	});
	let lines = text.split('\n').filter(|x| !x.is_empty()).collect::<Vec<_>>();
	let mut failures = lines
		.iter()
		.copied()
		.map_windows(|x: &[&str; 26]| {
			if x[25].contains("For full logs, run") {
				Some(x.to_vec())
			} else {
				None
			}
		})
		.flatten()
		.chain(
			if lines.len() < 26 && lines.last().unwrap().contains("For full logs, run") {
				Some(lines.clone())
			} else {
				None
			},
		)
		.map(|mut x| {
			let full_logs: Vec<_> = x
				.iter()
				.enumerate()
				.filter(|x| x.1.contains("For full logs, run"))
				.collect();
			if full_logs.len() >= 2 {
				// very short build log
				x = x[full_logs[full_logs.len() - 2].0 + 1..].to_vec();
			}
			x.into_iter()
				.flat_map(|x| {
					// indent is different depending on top-level vs. dependency fail
					x.strip_prefix("       > ")
						.or(x.strip_prefix("      > "))
						.or(x.strip_prefix("       For full logs, run "))
						.or(x.strip_prefix("      For full logs, run "))
				})
				.chain(Some("\n"))
		})
		.flatten()
		.map(|x| x.to_owned())
		.collect::<Vec<_>>();
	for line_orig in &mut failures {
		let Some(line) = line_orig.strip_prefix("'nix log ") else {
			continue;
		};
		let Some(line) = line.strip_suffix("'.") else {
			continue;
		};
		let nix_output = Command::new("nix")
			.current_dir("/tmp")
			.args(&["log", line])
			.stdout(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await?;
		let stripped = strip_ansi_escapes::strip(&nix_output.stdout);
		let s = String::from_utf8_lossy(&stripped);
		let url = Box::pin(paste(
			&format!("{title_prefix} - {}", &line[44..line.len() - 4]),
			title_prefix,
			&s,
		))
		.await?;
		line_orig.pop();
		*line_orig += ": ";
		*line_orig += &url;
	}
	if text.len() >= 5 * 1000 * 1000 {
		// truncate
		text = &text[text.ceil_char_boundary(text.len() - 5 * 1000 * 1000)..];
	}
	if !failures.is_empty() {
		map.as_object_mut()
			.unwrap()
			.insert("text".to_owned(), format!("{}", failures.join("\n")).into());
	} else {
		map.as_object_mut().unwrap().insert("text".to_owned(), text.into());
	}

	let client = reqwest::Client::new();
	let res = client.post(&*WASTEBIN_URL).json(&map).send().await?;
	let res: serde_json::Value = res.json().await?;
	Ok(format!(
		"{}{}",
		*WASTEBIN_URL,
		res.get("path").unwrap().as_str().unwrap()
	))
}
