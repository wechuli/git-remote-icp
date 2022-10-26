# git-remote-ic

![](https://img.shields.io/badge/status%EF%B8%8F-experimental-blueviolet)

A Git remote helper for the Internet Computer.

## Usage

1. Install to a location that is in your `PATH`.
2. Use `git` as you normally would, but use `ic://` instead of `https://` in URLs.


## Generating a public/private key pair

```
ssh-keygen -t rsa -b 4096 -C "0+a@users.noreply.codebase.org"
```

## Resources

* https://git-scm.com/docs/gitremote-helpers
* https://rovaughn.github.io/2015-2-9.html

## Debugging

### Against a local repository

```
cargo build && PATH=./target/debug:$PATH RUST_LOG=trace git clone ic::http://rwlgt-iiaaa-aaaaa-aaaaa-cai.raw.ic0.localhost:8453/@paul/hello-world.git
```

### Against a remote repository

```
cargo build && PATH=./target/debug:$PATH RUST_LOG=trace git clone ic://w7uni-tiaaa-aaaam-qaydq-cai.raw.ic0.app/@paul/hello-world.git
```

### By manually invoking the remote helper

```
cargo build && PATH=./target/debug:$PATH RUST_LOG=trace GIT_DIR=~/temp/hello-world git-remote-ic origin http://rwlgt-iiaaa-aaaaa-aaaaa-cai.raw.ic0.localhost:8453/@paul/hello-world.git
```

or, without rebuilding:

```
RUST_LOG=trace GIT_DIR=~/temp/hello-world cargo run origin http://rwlgt-iiaaa-aaaaa-aaaaa-cai.raw.ic0.localhost:8453/@paul/hello-world.git
```
