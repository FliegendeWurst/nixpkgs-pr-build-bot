#![feature(round_char_boundary)]

use std::{collections::HashSet, env, error::Error, process::Stdio, sync::Arc};

use frankenstein::{AsyncApi, AsyncTelegramApi, GetUpdatesParams, Message, SendMessageParams, UpdateContent};
use once_cell::sync::Lazy;
use tokio::{
	process::Command,
	sync::{Mutex, Semaphore},
	task,
};

static TASKS_RUNNING: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(10));
static PRS_RUNNING: Lazy<Mutex<HashSet<u32>>> = Lazy::new(|| Mutex::new(HashSet::new()));

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
					.text($txt)
					.build(),
			)
			.await?;
		};
	}

	// Fetch new updates via long poll method
	let mut update_params = GetUpdatesParams::builder().build();
	loop {
		let stream = api.get_updates(&update_params).await?;
		for update in stream.result {
			update_params.offset = Some(i64::from(update.update_id) + 1);
			// If the received update contains a new message...
			if let UpdateContent::Message(message) = update.content {
				if let Some(data) = &message.text {
					let Some((command, args)) = data.split_once(' ') else {
						reply!(message, format!("error: message not conformant to command syntax"));
						continue;
					};
					let args = args.to_owned();

					// Print received text message to stdout.
					// println!("<{}>: {}", &message.from.as_ref().map(|x| x.first_name.clone()).unwrap_or_default(), data);

					match command {
						"/pr" => {
							let api = Arc::clone(&api);
							task::spawn(async move {
								let id = message.chat.id;
								if let Err(e) = process_pr(Arc::clone(&api), message, args).await {
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
							});
						},
						_ => {
							reply!(message, format!("error: unknown command"));
						},
					}
				}
			}
		}
	}
}

async fn process_pr(api: Arc<AsyncApi>, msg: Message, args: String) -> Result<(), anyhow::Error> {
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
	let (num, mut pkgs) = if args.contains(' ') {
		let (num, pkgs) = args.split_once(' ').unwrap();
		(num.parse().unwrap_or(0), pkgs.split_whitespace().collect())
	} else {
		(args.parse().unwrap_or(0), vec![])
	};
	let mut prs = PRS_RUNNING.lock().await;
	if prs.contains(&num) {
		reply!(msg, "🤖 already processing that PR");
		return Ok(());
	}
	prs.insert(num);
	drop(prs);
	reply!(msg, format!("⏳ PR {num}, fetching ..."));

	let tmp = format!("/tmp/nixpkgs-{num}");
	Command::new("git")
		.current_dir("/home/arne/nixpkgs-wt-2")
		.args(["worktree", "add", &tmp])
		.spawn()?
		.wait()
		.await?;

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
		.args(["checkout", &rev])
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
	println!("PR {num}, changes:\n{out}");
	reply!(msg, format!("⏳ PR {num}, changes:\n{out}"));
	let lines = out.split('\n').filter(|x| !x.is_empty()).collect::<Vec<_>>();
	let mut doit = true;
	for line in &lines {
		let id = line.split_once(' ').unwrap().0;
		let cp = Command::new("git")
			.current_dir(&tmp)
			.args(format!("cherry-pick -x {id}").split(' '))
			.spawn()?
			.wait()
			.await?;
		if !cp.success() {
			reply!(msg, format!("😢 PR {num}, cherry-pick of {id} failed"));
			doit = false;
			break;
		}
	}

	for line in &lines {
		if line.is_empty() {
			continue;
		}
		let Some((pkg, _)) = line.split_once(' ').map(|x| x.1.split_once(": ")).flatten() else {
			continue;
		};
		if pkg.starts_with("nixos") || pkg.starts_with('-') || pkg.contains(' ') || pkg.starts_with("maintainers") {
			continue;
		}
		pkgs.push(pkg);
	}

	pkgs.sort();
	pkgs.dedup();
	let pkgs_to_build = pkgs.join(" ");
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
			.args(nix_args)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()?
			.wait_with_output()
			.await?;
		if nix_output.status.success() {
			reply!(msg, format!("✅ PR {num}, built successfully"));
		} else {
			let stdout = String::from_utf8_lossy(&nix_output.stderr);
			if stdout.len() < 1000 {
				reply!(msg, format!("💥 PR {num}, build failed"));
				reply!(msg, stdout);
			} else {
				let mut text = stdout.as_ref();
				if text.len() >= 5 * 1000 * 1000 {
					// truncate
					text = &text[text.ceil_char_boundary(text.len() - 5 * 1000 * 1000)..];
				}
				let map = serde_json::json!({
					"text": text,
					"extension": "log",
					"expires": 7 * 24 * 60 * 60
				});

				let client = reqwest::Client::new();
				let res = client
					.post("https://paste.fliegendewurst.eu/")
					.json(&map)
					.send()
					.await?;
				let res: serde_json::Value = res.json().await?;
				reply!(
					msg,
					format!(
						"💥 PR {num}, build failed\n👉 Full log: https://paste.fliegendewurst.eu{}",
						res.get("path").unwrap().as_str().unwrap()
					)
				);
			}
		}
	}

	Command::new("git")
		.current_dir("/home/arne/nixpkgs-wt-2")
		.args(["worktree", "remove", "--force", &tmp])
		.spawn()?
		.wait()
		.await?;
	Command::new("git")
		.current_dir("/home/arne/nixpkgs-wt-2")
		.args(["branch", "-D", &format!("nixpkgs-{num}")])
		.spawn()?
		.wait()
		.await?;

	let mut prs = PRS_RUNNING.lock().await;
	prs.remove(&num);
	drop(prs);

	drop(ticket);

	Ok(())
}
