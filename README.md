# nixpkgs PR build bot

This bot is essentially a private [ofborg](https://github.com/NixOS/ofborg?tab=readme-ov-file#ofborg), but much easier to host.
It builds all packages mentioned in the commits of a nixpkgs pull request.

## Configuration

You can configure the bot by setting certain environment variables.
The bot will process `$TASKS` PRs in parallel, with `$JOBS` jobs each, with each job getting `$CORES` cores.
`$NIXPKGS_DIRECTORY` must be set to a jj-colocated checkout of nixpkgs.
`$SUDO_PASSWORD` is used to switch to the `nobody` user when invoking Nix.
`$WASTEBIN` is the URL to a [wastebin](https://github.com/matze/wastebin) instance.
(Note, the instance must support [setting paste titles](https://github.com/matze/wastebin/pull/91).)
`$CONTACT` is displayed in the bot's introduction message.

`.env` template, with the defaults explicitly set:
```
export TELEGRAM_BOT_TOKEN=
export LANG=
export NIXPKGS_PR_BUILD_BOT_NIXPKGS_DIRECTORY=
export NIXPKGS_PR_BUILD_BOT_SUDO_PASSWORD=
export NIXPKGS_PR_BUILD_BOT_CORES=0
export NIXPKGS_PR_BUILD_BOT_JOBS=6
export NIXPKGS_PR_BUILD_BOT_TASKS=10
export NIXPKGS_PR_BUILD_BOT_WASTEBIN=https://paste.fliegendewurst.eu
export NIXPKGS_PR_BUILD_BOT_CONTACT=this bot's owner
```

## Usage

Start a chat with the bot. The bot will reply with a small introduction.

TL;DR: send `/pr <num>`.
