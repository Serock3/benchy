use std::collections::BTreeSet;

use benchy_lib::{Benchmark, BenchmarkGroup, BenchmarkStatus, MetricId, Unit};
use egui::{Color32, RichText, Ui, ahash::HashSet};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, Points};

use crate::provider::HttpProvider;

pub struct App {
    provider: HttpProvider,
    open_benchmarks: HashSet<BenchmarkGroup>,
}

impl App {
    pub fn new(context: &eframe::CreationContext<'_>) -> Self {
        Self {
            provider: HttpProvider::new(context.egui_ctx.clone()),
            open_benchmarks: HashSet::default(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.provider.update();

        egui::TopBottomPanel::top("top_panel").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Benchy");
                if ui
                    .add_enabled(!self.provider.is_loading(), egui::Button::new("Refresh"))
                    .clicked()
                {
                    self.provider.refresh();
                }
                if self.provider.is_loading() {
                    ui.spinner();
                }
                egui::widgets::global_theme_preference_buttons(ui);
            });
            if let Some(error) = self.provider.error() {
                ui.colored_label(Color32::RED, error);
            }
        });

        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("Benchmarks");
            if self.provider.benchmarks().is_empty() && !self.provider.is_loading() {
                ui.label("No benchmark results are stored yet.");
            }
            let table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto())
                .column(Column::auto())
                .column(Column::remainder())
                .column(Column::auto())
                .column(Column::auto());

            table
                .header(22.0, |mut header| {
                    for name in ["Repository", "Benchmark", "Description", "Latest", ""] {
                        header.col(|ui| {
                            ui.strong(name);
                        });
                    }
                })
                .body(|mut body| {
                    for (group, runs) in self.provider.benchmarks() {
                        body.row(20.0, |mut row| {
                            row.col(|ui| {
                                ui.label(&group.repository);
                            });
                            row.col(|ui| {
                                ui.label(&group.name);
                            });
                            row.col(|ui| {
                                ui.label(runs.last().map_or("", |run| run.description.as_str()));
                            });
                            row.col(|ui| {
                                ui.label(
                                    runs.last()
                                        .map(|run| {
                                            run.date.format("%Y-%m-%d %H:%M UTC").to_string()
                                        })
                                        .unwrap_or_default(),
                                );
                            });
                            row.col(|ui| {
                                if ui.button("Open").clicked() {
                                    self.open_benchmarks.insert(group.clone());
                                }
                            });
                        });
                    }
                });
        });

        self.open_benchmarks.retain(|group| {
            let Some(runs) = self.provider.benchmarks().get(group) else {
                return false;
            };
            let mut open = true;
            egui::Window::new(format!("{}/{}", group.repository, group.name))
                .open(&mut open)
                .default_width(1100.0)
                .show(context, |ui| benchmark_details(ui, group, runs));
            open
        });
    }
}

fn benchmark_details(ui: &mut Ui, group: &BenchmarkGroup, runs: &[Benchmark]) {
    if let Some(latest) = runs.last() {
        ui.label(&latest.description);
    }

    let units = runs
        .iter()
        .flat_map(|run| {
            run.measurements
                .values()
                .map(|measurement| measurement.unit)
        })
        .collect::<BTreeSet<_>>();
    for unit in units {
        ui.separator();
        ui.strong(unit.as_str().replace('_', " "));
        draw_plot(ui, group, runs, unit);
    }

    ui.separator();
    draw_runs_table(ui, runs);
}

fn draw_runs_table(ui: &mut Ui, runs: &[Benchmark]) {
    let metric_ids = runs
        .iter()
        .flat_map(|run| run.measurements.keys().cloned())
        .collect::<BTreeSet<_>>();
    let table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto());
    let table = metric_ids
        .iter()
        .fold(table, |table, _| table.column(Column::auto()));

    table
        .header(24.0, |mut header| {
            for name in ["Date", "Branch", "Commit", "Status"] {
                header.col(|ui| {
                    ui.strong(name);
                });
            }
            for metric_id in &metric_ids {
                header.col(|ui| {
                    ui.strong(metric_label(runs, metric_id))
                        .on_hover_text(metric_id.as_str());
                });
            }
        })
        .body(|mut body| {
            for run in runs.iter().rev() {
                body.row(20.0, |mut row| {
                    row.col(|ui| {
                        ui.label(run.date.format("%Y-%m-%d %H:%M").to_string());
                    });
                    row.col(|ui| {
                        ui.label(&run.branch);
                    });
                    row.col(|ui| {
                        let short_commit = run.commit.chars().take(12).collect::<String>();
                        ui.label(short_commit)
                            .on_hover_text(run.commit_message.as_deref().unwrap_or(&run.commit));
                    });
                    row.col(|ui| {
                        let label = match run.status {
                            BenchmarkStatus::Success => {
                                RichText::new("success").color(Color32::GREEN)
                            }
                            BenchmarkStatus::Failed => RichText::new("failed").color(Color32::RED),
                        };
                        ui.label(label)
                            .on_hover_text(run.error.as_deref().unwrap_or(""));
                    });
                    for metric_id in &metric_ids {
                        row.col(|ui| {
                            if let Some(measurement) = run.measurements.get(metric_id) {
                                ui.label(measurement.format_f64()(measurement.value));
                            }
                        });
                    }
                });
            }
        });
}

fn draw_plot(ui: &mut Ui, group: &BenchmarkGroup, runs: &[Benchmark], unit: Unit) {
    let metric_ids = runs
        .iter()
        .flat_map(|run| {
            run.measurements
                .iter()
                .filter(|(_, measurement)| measurement.unit == unit)
                .map(|(id, _)| id.clone())
        })
        .collect::<BTreeSet<_>>();

    let colors = [
        Color32::LIGHT_BLUE,
        Color32::LIGHT_GREEN,
        Color32::LIGHT_RED,
        Color32::GOLD,
        Color32::LIGHT_GRAY,
        Color32::MAGENTA,
    ];
    let mut lines = Vec::new();
    let mut point_sets = Vec::new();
    for (index, metric_id) in metric_ids.iter().enumerate() {
        let color = colors[index % colors.len()];
        let name = metric_label(runs, metric_id);
        let points = runs
            .iter()
            .enumerate()
            .filter_map(|(index, run)| {
                run.measurements
                    .get(metric_id)
                    .filter(|measurement| measurement.unit == unit)
                    .map(|measurement| [index as f64, measurement.value])
            })
            .collect::<Vec<_>>();
        lines.push(Line::new(name.clone(), points.clone()).color(color));
        point_sets.push(Points::new(name, points).color(color).radius(4.0_f32));
    }

    let formatter = unit.format_f64();
    Plot::new(("benchmark_plot", group, unit.as_str()))
        .height(260.0)
        .legend(Legend::default())
        .y_axis_formatter(|mark, _| formatter(mark.value))
        .label_formatter(|name, point| {
            let date = runs
                .get(point.x.round() as usize)
                .map(|run| run.date.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            format!("{name}\n{date}\n{}", formatter(point.y))
        })
        .show(ui, |plot_ui| {
            for line in lines {
                plot_ui.line(line);
            }
            for points in point_sets {
                plot_ui.points(points);
            }
        });
}

fn metric_label(runs: &[Benchmark], metric_id: &MetricId) -> String {
    runs.iter()
        .rev()
        .find_map(|run| run.measurements.get(metric_id))
        .map_or_else(
            || metric_id.as_str().to_owned(),
            |measurement| measurement.label.clone(),
        )
}
