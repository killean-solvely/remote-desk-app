#[allow(dead_code)]
#[path = "../engine.rs"]
mod engine;
#[allow(dead_code)]
#[path = "../model.rs"]
mod model;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = if args.len() == 3 {
        engine::provision(&args[1], &args[2])
    } else {
        Err("Usage: remote-desk-credentials REMOTE_ID ABSOLUTE_RCLONE_PATH".into())
    };
    match result {
        Ok(()) => println!("Password stored in the OS credential store."),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
