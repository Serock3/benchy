use std::{env, path::PathBuf};

fn data_directory() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[rocket::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = env::var_os("BENCHY_DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_directory().join("benchy/results.sqlite3"));
    let frontend = env::var_os("BENCHY_FRONTEND_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("benchy-gui/dist"));

    benchy_serve::rocket(database, frontend).launch().await?;
    Ok(())
}
