// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `grok-alab [grok arguments…]`: run aterm's managed Grok Build under the harness.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let status = grok_alab::run(args, &grok_alab::env::Env::process());
    grok_alab::signals::end_like(status)
}
