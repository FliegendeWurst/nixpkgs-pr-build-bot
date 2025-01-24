# nixpkgs PR build bot

<img align="right" src="https://github.com/user-attachments/assets/0661ad84-7fdb-4d71-b39f-fb4b2c65510e">

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
`$CONTACT`, `$DESCRIPTION` are displayed in the bot's introduction message.
`$GH_TOKEN` is only used for fetching evaluation artifacts, use a fine-grained access token with public read-only permissions.
`$CONFIG` is where certain settings are stored, if not set those features don't work.
`$CROSS` is used for automatic cross builds, if enabled.

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
export NIXPKGS_PR_BUILD_BOT_CONFIG=
export NIXPKGS_PR_BUILD_BOT_CROSS=aarch64-multiplatform
```

### NixOS config

Adapt path to `.env`, host platform and user.
```
systemd.services.prBuildBot = {
  description = "nixpkgs PR build bot";
  script = ''
    export PATH="$PATH:/run/wrappers/bin"
    jj --version
    git --version
    nix-shell --version
    sudo --version
    source .../path/to/.env
    exec ${lib.getExe nixpkgs-pr-build-bot.packages.x86_64-linux.nixpkgs-pr-build-bot}
  '';
  serviceConfig = {
    User = "xxx";
    Type = "simple";
    OOMPolicy = "continue";
  };
  path = with pkgs; [
    jujutsu
    git
    pkgs.nix
  ];
  wants = [ "network-online.target" ];
  wantedBy = [ "network-online.target" ];
  after = [ "network-online.target" ];
};
```

## Usage

Start a chat with the bot. The bot will reply with a small introduction.

TL;DR: send `/pr <num>` to build mentioned packages.
Use `/full <num>` to build all changed packages (equivalent to nixpkgs-review).

## Maintenance

To clean up testing branches use this script.
Recommend backing up your nixpkgs working directory first, in case of jj log failures.

```sh
#!/bin/sh

set -euo pipefail

time_now=$(date '+%s')
for c in $(git for-each-ref --sort=committerdate 'refs/heads/nixpkgs-*' --format='%(refname)')
do
    time_touched=$(git show -s --format="%ct" "$c")
    [[ 172800 -lt $(($time_now - $time_touched)) ]] || break
    git show -s --format="%ci $c" "$c"
    num=$(echo $c | rg --only-matching '\d+')
    revs=$(jj log --no-pager --no-graph -T 'change_id ++ " "' -r "..nixpkgs-$num ~ ..@")
    jj abandon $revs
    jj bookmark delete "nixpkgs-$num"
done
```
