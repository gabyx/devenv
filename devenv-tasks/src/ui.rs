use console::Term;
use std::path::PathBuf;
use std::sync::Arc;

use crate::types::{Skipped, TaskCompleted, TaskStatus};
use crate::{Config, Error, Outputs, Tasks, VerbosityLevel};

/// Status information for all tasks
pub struct TasksStatus {
    lines: Vec<String>,
    pub pending: usize,
    pub running: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub dependency_failed: usize,
}

impl TasksStatus {
    fn new() -> Self {
        Self {
            lines: vec![],
            pending: 0,
            running: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            dependency_failed: 0,
        }
    }
}

/// UI manager for tasks
pub struct TasksUi {
    tasks: Arc<Tasks>,
    verbosity: VerbosityLevel,
    term: Term,
}

impl TasksUi {
    /// Create a new TasksUi
    pub async fn new(config: Config, verbosity: VerbosityLevel) -> Result<Self, Error> {
        let tasks = Tasks::new(config, verbosity).await?;

        Ok(Self {
            tasks: Arc::new(tasks),
            verbosity,
            term: Term::stderr(),
        })
    }

    /// Create a new TasksUi with a specific database path
    pub async fn new_with_db_path(
        config: Config,
        db_path: PathBuf,
        verbosity: VerbosityLevel,
    ) -> Result<Self, Error> {
        let tasks = Tasks::new_with_db_path(config, db_path, verbosity).await?;

        Ok(Self {
            tasks: Arc::new(tasks),
            verbosity,
            term: Term::stderr(),
        })
    }

    async fn get_tasks_status(&self) -> TasksStatus {
        let mut tasks_status = TasksStatus::new();

        for index in &self.tasks.tasks_order {
            let (task_status, task_name) = {
                let task_state = self.tasks.graph[*index].read().await;
                (task_state.status.clone(), task_state.task.name.clone())
            };
            let (status_text, duration) = match task_status {
                TaskStatus::Pending => {
                    tasks_status.pending += 1;
                    continue;
                }
                TaskStatus::Running(started) => {
                    tasks_status.running += 1;
                    (
                        console::style(format!("{:17}", "Running")).blue().bold(),
                        Some(started.elapsed()),
                    )
                }
                TaskStatus::Completed(TaskCompleted::Skipped(skipped)) => {
                    tasks_status.skipped += 1;
                    let status = match skipped {
                        Skipped::Cached(_) => "Cached",
                        Skipped::NotImplemented => "Not implemented",
                    };
                    (console::style(format!("{:17}", status)).blue().bold(), None)
                }
                TaskStatus::Completed(TaskCompleted::Success(duration, _)) => {
                    tasks_status.succeeded += 1;
                    (
                        console::style(format!("{:17}", "Succeeded")).green().bold(),
                        Some(duration),
                    )
                }
                TaskStatus::Completed(TaskCompleted::Failed(duration, _)) => {
                    tasks_status.failed += 1;
                    (
                        console::style(format!("{:17}", "Failed")).red().bold(),
                        Some(duration),
                    )
                }
                TaskStatus::Completed(TaskCompleted::DependencyFailed) => {
                    tasks_status.dependency_failed += 1;
                    (
                        console::style(format!("{:17}", "Dependency failed"))
                            .magenta()
                            .bold(),
                        None,
                    )
                }
            };

            let duration = match duration {
                Some(d) => d.as_millis().to_string() + "ms",
                None => "".to_string(),
            };

            tasks_status.lines.push(format!(
                "{} {:40} {:10}",
                status_text,
                console::style(task_name).bold(),
                duration
            ));
        }

        tasks_status
    }

    /// Run all tasks
    pub async fn run(&mut self) -> Result<(TasksStatus, Outputs), Error> {
        let tasks_clone = Arc::clone(&self.tasks);
        let handle = tokio::spawn(async move { tasks_clone.run().await });

        // If in quiet mode, just wait for tasks to complete
        if self.verbosity == VerbosityLevel::Quiet {
            loop {
                let tasks_status = self.get_tasks_status().await;
                if tasks_status.pending == 0 && tasks_status.running == 0 {
                    break;
                }
            }

            // Print errors even in quiet mode
            let errors = self.format_task_errors().await;
            if !errors.is_empty() {
                let styled_errors = console::Style::new().apply_to(errors);
                self.console_write_line(&styled_errors.to_string())?;
            }

            let tasks_status = self.get_tasks_status().await;
            return Ok((tasks_status, handle.await.unwrap()));
        }

        let names = console::style(self.tasks.root_names.join(", ")).bold();

        // Disable TUI in verbose mode to prevent it from overwriting task output
        let is_tty = self.term.is_term() && self.verbosity != VerbosityLevel::Verbose;

        // Always show which tasks are being run
        self.console_write_line(&format!("{:17} {}\n", "Running tasks", names))?;

        // start processing tasks
        let started = std::time::Instant::now();

        // start TUI if we're connected to a TTY and not in verbose mode, otherwise use non-interactive output
        // This prevents the TUI from overwriting stdout/stderr in verbose mode
        let mut last_list_height: u16 = 0;
        let mut last_statuses = std::collections::HashMap::new();

        loop {
            let tasks_status = self.get_tasks_status().await;
            let status_summary = [
                if tasks_status.pending > 0 {
                    format!(
                        "{} {}",
                        tasks_status.pending,
                        console::style("Pending").blue().bold()
                    )
                } else {
                    String::new()
                },
                if tasks_status.running > 0 {
                    format!(
                        "{} {}",
                        tasks_status.running,
                        console::style("Running").blue().bold()
                    )
                } else {
                    String::new()
                },
                if tasks_status.skipped > 0 {
                    format!(
                        "{} {}",
                        tasks_status.skipped,
                        console::style("Skipped").blue().bold()
                    )
                } else {
                    String::new()
                },
                if tasks_status.succeeded > 0 {
                    format!(
                        "{} {}",
                        tasks_status.succeeded,
                        console::style("Succeeded").green().bold()
                    )
                } else {
                    String::new()
                },
                if tasks_status.failed > 0 {
                    format!(
                        "{} {}",
                        tasks_status.failed,
                        console::style("Failed").red().bold()
                    )
                } else {
                    String::new()
                },
                if tasks_status.dependency_failed > 0 {
                    format!(
                        "{} {}",
                        tasks_status.dependency_failed,
                        console::style("Dependency Failed").red().bold()
                    )
                } else {
                    String::new()
                },
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

            if is_tty {
                let elapsed_time = format!("{:.2?}", started.elapsed());

                let output = format!(
                    "{}\n{status_summary}{}{elapsed_time}",
                    tasks_status.lines.join("\n"),
                    " ".repeat(
                        (19 + self.tasks.longest_task_name)
                            .saturating_sub(console::measure_text_width(&status_summary))
                            .max(1)
                    )
                );
                if !tasks_status.lines.is_empty() {
                    let output = console::Style::new().apply_to(output);
                    if last_list_height > 0 {
                        self.term.move_cursor_up(last_list_height as usize)?;
                        self.term.clear_to_end_of_screen()?;
                    }
                    self.console_write_line(&output.to_string())?;
                }

                last_list_height = tasks_status.lines.len() as u16 + 1;
            } else {
                // Non-interactive mode - print only status changes
                for task_state in self.tasks.graph.node_weights() {
                    let task_state = task_state.read().await;
                    let task_name = &task_state.task.name;
                    let current_status = match &task_state.status {
                        TaskStatus::Pending => "Pending".to_string(),
                        TaskStatus::Running(_) => {
                            if let Some(previous) = last_statuses.get(task_name) {
                                if previous != "Running" {
                                    self.console_write_line(&format!(
                                        "{:17} {}",
                                        console::style("Running").blue().bold(),
                                        console::style(task_name).bold()
                                    ))?;
                                }
                            } else {
                                self.console_write_line(&format!(
                                    "{:17} {}",
                                    console::style("Running").blue().bold(),
                                    console::style(task_name).bold()
                                ))?;
                            }
                            "Running".to_string()
                        }
                        TaskStatus::Completed(completed) => {
                            let (status, style, duration_str) = match completed {
                                TaskCompleted::Success(duration, _) => (
                                    format!("Succeeded ({:.2?})", duration),
                                    console::style("Succeeded").green().bold(),
                                    format!(" ({:.2?})", duration),
                                ),
                                TaskCompleted::Skipped(Skipped::Cached(_)) => (
                                    "Cached".to_string(),
                                    console::style("Cached").blue().bold(),
                                    "".to_string(),
                                ),
                                TaskCompleted::Skipped(Skipped::NotImplemented) => (
                                    "Not implemented".to_string(),
                                    console::style("Not implemented").blue().bold(),
                                    "".to_string(),
                                ),
                                TaskCompleted::Failed(duration, _) => (
                                    format!("Failed ({:.2?})", duration),
                                    console::style("Failed").red().bold(),
                                    format!(" ({:.2?})", duration),
                                ),
                                TaskCompleted::DependencyFailed => (
                                    "Dependency failed".to_string(),
                                    console::style("Dependency failed").red().bold(),
                                    "".to_string(),
                                ),
                            };

                            if let Some(previous) = last_statuses.get(task_name) {
                                if previous != &status {
                                    self.console_write_line(&format!(
                                        "{:17} {}{}",
                                        style,
                                        console::style(task_name).bold(),
                                        duration_str
                                    ))?;
                                }
                            } else {
                                self.console_write_line(&format!(
                                    "{:17} {}{}",
                                    style,
                                    console::style(task_name).bold(),
                                    duration_str
                                ))?;
                            }
                            status
                        }
                    };

                    last_statuses.insert(task_name.clone(), current_status);
                }
            }

            // Break early if there are no more tasks left
            if tasks_status.pending == 0 && tasks_status.running == 0 {
                if !is_tty {
                    self.console_write_line(&status_summary)?;
                }
                break;
            }

            // Wait for task updates before looping
            self.tasks.notify_ui.notified().await;
        }

        let errors = self.format_task_errors().await;
        if !errors.is_empty() {
            let styled_errors = console::Style::new().apply_to(errors);
            self.console_write_line(&styled_errors.to_string())?;
        }

        let tasks_status = self.get_tasks_status().await;
        Ok((tasks_status, handle.await.unwrap()))
    }

    fn console_write_line(&self, message: &str) -> std::io::Result<()> {
        self.term.write_line(message)?;
        Ok(())
    }

    /// Format error messages from failed tasks
    async fn format_task_errors(&self) -> String {
        let mut errors = String::new();
        for index in &self.tasks.tasks_order {
            let task_state = self.tasks.graph[*index].read().await;
            if let TaskStatus::Completed(TaskCompleted::Failed(_, failure)) = &task_state.status {
                errors.push_str(&format!(
                    "\n--- {} failed with error: {}\n",
                    task_state.task.name, failure.error
                ));
                errors.push_str(&format!("--- {} stdout:\n", task_state.task.name));
                for (time, line) in &failure.stdout {
                    errors.push_str(&format!(
                        "{:07.2}: {}\n",
                        time.elapsed().as_secs_f32(),
                        line
                    ));
                }
                errors.push_str(&format!("--- {} stderr:\n", task_state.task.name));
                for (time, line) in &failure.stderr {
                    errors.push_str(&format!(
                        "{:07.2}: {}\n",
                        time.elapsed().as_secs_f32(),
                        line
                    ));
                }
                errors.push_str("---\n")
            }
        }
        errors
    }
}
