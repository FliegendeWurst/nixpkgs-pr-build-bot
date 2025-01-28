use std::process::Stdio;

use tokio::process::Command;

use crate::WASTEBIN_URL;

pub async fn paste(title: &str, title_prefix: &str, mut text: &str) -> Result<String, anyhow::Error> {
	let oom = text.contains("No space left on device");
	let mut map = serde_json::json!({
		"title": if !oom { title.to_owned() } else { format!("{title} - likely out of /tmp space") },
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
	failures.extend(
		lines
			.iter()
			.filter(|x| x.starts_with("error: builder for ") && x.contains("failed to produce output path for output"))
			.map(|&x| {
				let drv_path = x
					.strip_prefix("error: builder for '")
					.unwrap_or("???")
					.split_once("' failed to produce")
					.unwrap_or(("???", ""))
					.0;
				vec![x.to_owned(), format!("'nix log {drv_path}'."), "\n".to_owned()].into_iter()
			})
			.flatten(),
	);
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
