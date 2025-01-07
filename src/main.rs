#![feature(round_char_boundary, iter_map_windows)]

use std::{
	collections::{HashMap, HashSet},
	env,
	error::Error,
	process::Stdio,
	sync::Arc,
	time::{Duration, SystemTime},
};

use frankenstein::{
	AllowedUpdate, AsyncApi, AsyncTelegramApi, GetUpdatesParams, LinkPreviewOptions, Message, SendMessageParams,
	UpdateContent,
};
use once_cell::sync::Lazy;
use tokio::{
	fs,
	process::Command,
	sync::{Mutex, Semaphore},
	task,
};

static TASKS_RUNNING: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(10));
static PRS_RUNNING: Lazy<Mutex<HashSet<u32>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static PRS_BUILDING: Lazy<Mutex<HashMap<u32, Vec<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static GIT_OPERATIONS: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[tokio::main]
async fn main() {
	real_main().await.unwrap();
}

async fn real_main() -> Result<(), Box<dyn Error>> {
	let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
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
		.allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::MessageReaction])
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
		for update in stream.result {
			update_params.offset = Some(i64::from(update.update_id) + 1);
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
				let (mut command, mut args) = data.split_once(' ').unwrap_or((data, ""));
				// reply!(message, format!("error: message not conformant to command syntax"));

				if let Some(pr_num) = command.strip_prefix("https://github.com/NixOS/nixpkgs/pull/") {
					command = "/pr";
					let pr_num = pr_num.split(|x: char| !x.is_digit(10)).next().unwrap();
					args = pr_num;
				}

				let args = args.to_owned();

				// Print received text message to stdout.
				// println!("<{}>: {}", &message.from.as_ref().map(|x| x.first_name.clone()).unwrap_or_default(), data);

				match command {
					"/start" => {
						reply!(message, "This bot allows you to build nixpkgs PRs.\nUsage: /pr <number> <packages> -<exclude packages>
You can also just send the PR as URL. Packages is a space seperated list of additional packages to build. You can exclude certain packages by prefixing them with -.
PRs are tested by cherry-picking their commits on a somewhat recent master/staging merge, with strictDeps and PIE enabled by default, and all of my own PRs merged.
Ping t.me/FliegendeWurst if you have trouble.");
					},
					"/pr" => {
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
							if let Err(e) = process_pr(Arc::clone(&api), message, num, pkgs).await {
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
							status += &format!("PR {num}: {}", pkgs.join(" "));
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

async fn process_pr(api: Arc<AsyncApi>, msg: Message, num: u32, mut pkgs: Vec<String>) -> Result<(), anyhow::Error> {
	macro_rules! reply {
		($msg:expr, $txt:expr) => {
			api.send_message(
				&SendMessageParams::builder()
					.chat_id($msg.chat.id)
					.text($txt)
					.build(),
			)
			.await?;
		};
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
	reply!(msg, format!("⏳ PR {num}, fetching ..."));

	let git_operations = GIT_OPERATIONS.lock().await;
	let rev = String::from_utf8(
		Command::new("jj")
			.current_dir("/home/arne/nixpkgs-wt-2")
			.args("log --limit 1 --no-graph -T commit_id -r @".split_whitespace())
			.stdout(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await?
			.stdout,
	)
	.unwrap();

	let tmp = format!("/tmp/nixpkgs-{num}");
	Command::new("git")
		.current_dir("/home/arne/nixpkgs-wt-2")
		.args(["worktree", "add", &tmp, &format!("{rev}~")])
		.spawn()?
		.wait()
		.await?;

	Command::new("git")
		.current_dir(&tmp)
		.args(["fetch", "origin", "master", "staging", "staging-next"])
		.spawn()?
		.wait()
		.await?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["fetch", "origin", &format!("pull/{num}/head")])
		.spawn()?
		.wait()
		.await?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["switch", "-C", &format!("nixpkgs-{num}")])
		.spawn()?
		.wait()
		.await?;
	Command::new("git")
		.current_dir(&tmp)
		.args(["restore", "-s", &rev, "--", "."])
		.spawn()?
		.wait()
		.await?;
	fs::write(
		format!("{tmp}/jujutsu_hack.txt"),
		format!(
			"dummy {num} {}",
			SystemTime::now()
				.duration_since(SystemTime::UNIX_EPOCH)
				.unwrap()
				.as_secs()
		),
	)
	.await?;
	Command::new("git")
		.current_dir(&tmp)
		.args("add .".split(' '))
		.spawn()?
		.wait()
		.await?;
	Command::new("git")
		.current_dir(&tmp)
		.args("commit -a --message wip".split(' '))
		.spawn()?
		.wait()
		.await?;
	let output = Command::new("git")
		.current_dir(&tmp)
		.args("log --oneline FETCH_HEAD --not origin/master origin/staging origin/staging-next".split(' '))
		.stdout(Stdio::piped())
		.spawn()?
		.wait_with_output()
		.await?;
	let out = String::from_utf8(output.stdout)?;
	let mut lines = out.split('\n').filter(|x| !x.is_empty()).collect::<Vec<_>>();
	lines.reverse();
	println!("PR {num}, changes:\n{}", lines.join("\n"));
	reply!(msg, format!("⏳ PR {num}, changes:\n{}", lines.join("\n")));
	let mut doit = true;
	let mut warn_merge = false;
	for line in &lines {
		let id = line.split_once(' ').unwrap().0;
		let cp = Command::new("git")
			.current_dir(&tmp)
			.args(format!("cherry-pick --allow-empty -x {id}").split(' '))
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await?;
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
				.spawn()?
				.wait_with_output()
				.await?;
			let output_diff = String::from_utf8_lossy(&diff.stdout);
			let url = paste(&format!(
				"git cherry-pick standard output:\n{output}\ngit cherry-pick standard error:\n{output_err}\ngit diff output:\n{output_diff}"
			))
			.await?;
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
	let to_remove = pkgs
		.iter()
		.flat_map(|x| x.strip_prefix('-'))
		.map(|x| x.to_owned())
		.collect::<Vec<_>>();
	for pkg in to_remove {
		pkgs.remove_item(pkg);
	}
	pkgs.retain(|x| !x.starts_with('-'));

	pkgs.sort();
	pkgs.dedup();
	let pkgs_to_build = pkgs.join(" ");
	let mut prs_building = PRS_BUILDING.lock().await;
	prs_building.insert(num, pkgs.clone());
	drop(prs_building);
	println!("PR {num}, building: {pkgs_to_build}");
	if doit {
		reply!(msg, format!("⏳ PR {num}, building: {pkgs_to_build}"));

		let mut nix_args = vec![
			"--run".to_owned(),
			"exit".to_owned(),
			"-k".to_owned(),
			"-j6".to_owned(),
			"-I".to_owned(),
			format!("nixpkgs={tmp}"),
			"-p".to_owned(),
		];
		for x in pkgs {
			nix_args.push(x.to_owned());
		}
		let nix_output = Command::new("nix-shell")
			.current_dir(&tmp)
			.env("NIXPKGS_ALLOW_UNFREE", "1")
			.env("NIXPKGS_ALLOW_INSECURE", "1")
			.args(nix_args)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await?;
		if nix_output.status.success() {
			if warn_merge {
				reply!(
					msg,
					format!("✅ PR {num}, built successfully\n⚠️ PR contains merge commits")
				);
			} else {
				reply!(msg, format!("✅ PR {num}, built successfully"));
			}
		} else {
			let stripped = strip_ansi_escapes::strip(&nix_output.stderr);
			let stdout = String::from_utf8_lossy(&stripped);
			if stdout.len() < 1000 {
				reply!(msg, format!("💥 PR {num}, build failed"));
				reply!(msg, stdout);
			} else {
				let text = stdout.as_ref();
				let url = paste(text).await?;
				reply!(msg, format!("💥 PR {num}, build failed\n👉 Full log: {url}",));
			}
		}
	}

	let git_operations = GIT_OPERATIONS.lock().await;
	Command::new("git")
		.current_dir("/home/arne/nixpkgs-wt-2")
		.args(["worktree", "remove", "--force", &tmp])
		.spawn()?
		.wait()
		.await?;
	//Command::new("git")
	//	.current_dir("/home/arne/nixpkgs-wt-2")
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

async fn paste(mut text: &str) -> Result<String, anyhow::Error> {
	let mut map = serde_json::json!({
		"extension": "log",
		"expires": 7 * 24 * 60 * 60
	});
	let mut failures = text
		.split('\n')
		.map_windows(|x: &[&str; 26]| {
			if x[25].contains("For full logs, run") {
				Some(*x)
			} else {
				None
			}
		})
		.flatten()
		.map(|x| {
			x.into_iter()
				.flat_map(|x| {
					x.strip_prefix("       > ")
						.or(x.strip_prefix("       For full logs, run "))
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
		let url = Box::pin(paste(&s)).await?;
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
			.insert("text".to_owned(), format!("{}\n\n{}", failures.join("\n"), text).into());
	} else {
		map.as_object_mut().unwrap().insert("text".to_owned(), text.into());
	}

	let client = reqwest::Client::new();
	let res = client
		.post("https://paste.fliegendewurst.eu/")
		.json(&map)
		.send()
		.await?;
	let res: serde_json::Value = res.json().await?;
	Ok(format!(
		"https://paste.fliegendewurst.eu{}",
		res.get("path").unwrap().as_str().unwrap()
	))
}
