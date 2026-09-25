use std::{collections::BTreeMap, sync::mpsc};

use benchy_lib::{Benchmark, BenchmarkGroup};
use ehttp::Request;

pub struct HttpProvider {
    context: egui::Context,
    sender: mpsc::Sender<Result<Vec<Benchmark>, String>>,
    receiver: mpsc::Receiver<Result<Vec<Benchmark>, String>>,
    benchmarks: BTreeMap<BenchmarkGroup, Vec<Benchmark>>,
    error: Option<String>,
    loading: bool,
}

impl HttpProvider {
    pub fn new(context: egui::Context) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut provider = Self {
            context,
            sender,
            receiver,
            benchmarks: BTreeMap::new(),
            error: None,
            loading: false,
        };
        provider.refresh();
        provider
    }

    pub fn refresh(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let sender = self.sender.clone();
        let context = self.context.clone();
        ehttp::fetch(Request::get("/api/benchmarks"), move |response| {
            let result = response
                .map_err(|error| format!("failed to fetch benchmark results: {error}"))
                .and_then(|response| {
                    response
                        .json::<Vec<Benchmark>>()
                        .map_err(|error| format!("failed to decode benchmark results: {error}"))
                });
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    pub fn update(&mut self) {
        if let Ok(result) = self.receiver.try_recv() {
            self.loading = false;
            match result {
                Ok(benchmarks) => {
                    let mut grouped = BTreeMap::<BenchmarkGroup, Vec<Benchmark>>::new();
                    for benchmark in benchmarks {
                        grouped
                            .entry(benchmark.group.clone())
                            .or_default()
                            .push(benchmark);
                    }
                    self.benchmarks = grouped;
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    pub fn benchmarks(&self) -> &BTreeMap<BenchmarkGroup, Vec<Benchmark>> {
        &self.benchmarks
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }
}
