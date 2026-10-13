//! The agent's end-to-end tests, in one binary so the agent is linked once
//! instead of once per file. Each test chooses its socket and folders with
//! environment variables, so they take turns (`common::serial`).

mod common;

mod commands;
mod files;
mod ide;
mod killed;
mod persistence;
mod relay;
mod replace;
mod responsiveness;
mod restart;
mod takeover;
