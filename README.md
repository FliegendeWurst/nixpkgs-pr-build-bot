# nixpkgs PR build bot

## Configuration

You can configure the bot by setting certain environment variables.
The bot will process `$TASKS` PRs in parallel, with `$JOBS` jobs each, with each job getting `$CORES` cores.
`$NIXPKGS_DIRECTORY` must be set to a jj-colocated checkout of nixpkgs.
`$SUDO_PASSWORD` is used to switch to the `nobody` user when invoking Nix.
`$WASTEBIN` is the URL to a [wastebin](https://github.com/matze/wastebin) instance.

Template for your `.env` file, with the defaults explicitly set:
```
export TELEGRAM_BOT_TOKEN=
export LANG=
export NIXPKGS_PR_BUILD_BOT_NIXPKGS_DIRECTORY=
export NIXPKGS_PR_BUILD_BOT_SUDO_PASSWORD=
export NIXPKGS_PR_BUILD_BOT_CORES=0
export NIXPKGS_PR_BUILD_BOT_JOBS=6
export NIXPKGS_PR_BUILD_BOT_TASKS=10
export NIXPKGS_PR_BUILD_BOT_WASTEBIN=https://paste.fliegendewurst.eu
```