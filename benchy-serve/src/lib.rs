use std::path::{Path, PathBuf};

use benchy_lib::Benchmark;
use benchy_store::Store;
use rocket::{
    Build, Rocket, State, fs::FileServer, http::Status, response::status::Custom, routes,
    serde::json::Json, tokio::task,
};

#[derive(Debug)]
struct ServerState {
    database: PathBuf,
}

type ApiResult<T> = Result<T, Custom<String>>;

#[rocket::get("/benchmarks")]
async fn benchmarks(state: &State<ServerState>) -> ApiResult<Json<Vec<Benchmark>>> {
    let database = state.database.clone();
    let benchmarks = task::spawn_blocking(move || {
        let store = Store::open_read_only(database)?;
        store.benchmarks()
    })
    .await
    .map_err(|error| internal_error(format!("database task failed: {error}")))?
    .map_err(|error| internal_error(format!("failed to query benchmarks: {error:#}")))?;
    Ok(Json(benchmarks))
}

#[rocket::get("/health")]
async fn health(state: &State<ServerState>) -> ApiResult<&'static str> {
    let database = state.database.clone();
    task::spawn_blocking(move || Store::open_read_only(database)?.latest_runs(1))
        .await
        .map_err(|error| internal_error(format!("database task failed: {error}")))?
        .map_err(|error| internal_error(format!("database is unavailable: {error:#}")))?;
    Ok("ok")
}

fn internal_error(message: String) -> Custom<String> {
    log::error!("{message}");
    Custom(Status::InternalServerError, message)
}

pub fn rocket(database: impl Into<PathBuf>, frontend: impl AsRef<Path>) -> Rocket<Build> {
    let frontend = frontend.as_ref();
    let rocket = rocket::build()
        .manage(ServerState {
            database: database.into(),
        })
        .mount("/api", routes![benchmarks, health]);

    if frontend.is_dir() {
        rocket.mount("/", FileServer::from(frontend))
    } else {
        log::warn!(
            "frontend directory {} does not exist; serving the API only",
            frontend.display()
        );
        rocket
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use benchy_lib::{
        Benchmark, BenchmarkGroup, BenchmarkStatus, Measurement, MetricId, SchemaVersion, Unit,
    };
    use benchy_store::Store;
    use rocket::{http::Status, local::asynchronous::Client};

    use super::rocket;

    #[rocket::async_test]
    async fn serves_results_from_sqlite() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("results.sqlite3");
        let result = Benchmark {
            schema_version: SchemaVersion,
            group: BenchmarkGroup {
                repository: "gotatun".to_owned(),
                name: "gotatun-throughput".to_owned(),
            },
            commit: "abc123".to_owned(),
            branch: "main".to_owned(),
            commit_message: None,
            description: "GotaTun throughput".to_owned(),
            date: "2026-09-25T12:00:00Z".parse().unwrap(),
            measurements: BTreeMap::from([(
                MetricId::new("throughput.receiver").unwrap(),
                Measurement {
                    label: "Receiver throughput".to_owned(),
                    unit: Unit::BitsPerSecond,
                    value: 2_500_000_000.0,
                },
            )]),
            run_id: Some("123456".to_owned()),
            status: BenchmarkStatus::Success,
            error: None,
            parameters: BTreeMap::new(),
            environment: BTreeMap::new(),
        };
        let mut store = Store::open(&database).unwrap();
        store.ingest(std::slice::from_ref(&result)).unwrap();

        let frontend = directory.path().join("frontend");
        std::fs::create_dir(&frontend).unwrap();
        std::fs::write(frontend.join("index.html"), "<h1>Benchy</h1>").unwrap();
        let client = Client::tracked(rocket(&database, &frontend)).await.unwrap();

        let response = client.get("/api/benchmarks").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(
            response.into_json::<Vec<Benchmark>>().await.unwrap(),
            [result]
        );
        let response = client.get("/api/health").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let response = client.get("/").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        assert!(response.into_string().await.unwrap().contains("Benchy"));
    }
}
