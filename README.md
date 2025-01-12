# nixpkgs PR build bot

This bot is essentially a private [ofborg](https://github.com/NixOS/ofborg?tab=readme-ov-file#ofborg) / [nixpkgs-review](https://github.com/Mic92/nixpkgs-review) bot, but much easier to host.
It builds all packages mentioned in the commits of a nixpkgs pull request, or alternatively all packages impacted by the changes.

Send me a message if you would like to access a demo instance of the bot.

## Configuration

You can configure the bot by setting certain environment variables.
The bot will process `$TASKS` PRs in parallel, with `$JOBS` jobs each, with each job getting `$CORES` cores.
`$NIXPKGS_DIRECTORY` must be set to a [jj](https://github.com/jj-vcs/jj#readme)-colocated checkout of nixpkgs.
`$SUDO_PASSWORD` is used to switch to the `nobody` user when invoking Nix.
`$WASTEBIN` is the URL to a [wastebin](https://github.com/matze/wastebin) instance.
(Note, the instance must support [setting paste titles](https://github.com/matze/wastebin/pull/91).)
`$CONTACT` is displayed in the bot's introduction message.
`$GH_TOKEN` is only used for fetching evaluation artifacts, use a fine-grained access token with public read-only permissions.

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
export NIXPKGS_PR_BUILD_BOT_CONTACT="this bot's owner"
export NIXPKGS_PR_BUILD_BOT_DESCRIPTION="the nixpkgs revision currently checked out"
export NIXPKGS_PR_BUILD_BOT_GH_TOKEN=
```

## Usage

Start a chat with the bot. The bot will reply with a small introduction.

TL;DR: send `/pr <num>` to build mentioned packages.
Use `/full <num>` to build all changed packages (equivalent to nixpkgs-review).
