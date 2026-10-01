//! Manual test: `cargo run -p client --example ssh_probe -- <server> <agents folder>`.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let start = std::time::Instant::now();
    let client = client::connect_ssh(&args[0], std::path::Path::new(&args[1]), &|step| eprintln!("{step}"))?;
    println!("connected in {:?}", start.elapsed());
    let response = smol::block_on(client.request(proto::Request::TaskList))?;
    println!("tasks: {response:?}");
    Ok(())
}
