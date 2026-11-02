//! Кооперативная отмена локального I/O: async-таймаут не останавливает worker.
//! Отмена и commit сериализованы; начатый системный вызов должен завершиться сам.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct WorkBudget {
    state: Arc<BudgetState>,
}

#[derive(Default)]
struct BudgetState {
    deadline: Option<Instant>,
    cancelled: AtomicBool,
    commit: Arc<Mutex<()>>,
    parent: Option<WorkBudget>,
}

pub struct CancelOnDrop(WorkBudget);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _commit = self.0.state.commit.lock().unwrap_or_else(|error| error.into_inner());
        self.0.state.cancelled.store(true, Ordering::Release);
    }
}

impl WorkBudget {
    pub fn within(duration: Duration) -> Self {
        Self { state: Arc::new(BudgetState {
            deadline: Some(Instant::now() + duration),
            ..BudgetState::default()
        }) }
    }

    pub fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }

    pub fn limited_to(&self, duration: Duration) -> Self {
        let deadline = Instant::now() + duration;
        Self { state: Arc::new(BudgetState {
            deadline: Some(self.state.deadline.map_or(deadline, |parent| parent.min(deadline))),
            commit: self.state.commit.clone(),
            parent: Some(self.clone()),
            ..BudgetState::default()
        }) }
    }

    pub fn check(&self) -> io::Result<()> {
        if let Some(parent) = &self.state.parent {
            parent.check()?;
        }
        if self.state.cancelled.load(Ordering::Acquire) {
            // read_exact повторяет Interrupted; отмена не должна запускать бесконечный retry.
            return Err(io::Error::other("установка сборки отменена"));
        }
        if self.state.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(io::ErrorKind::TimedOut,
                "сборка совместимости не успела установиться — попробуйте повторить"));
        }
        Ok(())
    }

    pub fn commit<T>(&self, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let _commit = self.state.commit.lock().map_err(|_| io::Error::other("commit сборки недоступен"))?;
        self.check()?;
        operation()
    }

    pub async fn blocking<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
        self.check()?;
        let budget = self.clone();
        let result = tokio::task::spawn_blocking(move || -> io::Result<T> {
            budget.check()?;
            Ok(work())
        }).await.map_err(|error| io::Error::other(format!("worker сборки: {error}")))??;
        self.check()?;
        Ok(result)
    }

    pub async fn network<T>(&self, future: impl Future<Output = T>) -> io::Result<T> {
        self.check()?;
        let result = if let Some(deadline) = self.state.deadline {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future).await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut,
                    "сборка совместимости не успела установиться — попробуйте повторить"))?
        } else {
            future.await
        };
        self.check()?;
        Ok(result)
    }

    pub fn reader<R>(&self, reader: R) -> BudgetReader<R> {
        BudgetReader { reader, budget: self.clone() }
    }

    pub fn copy(&self, input: &mut impl Read, output: &mut impl Write) -> io::Result<u64> {
        let mut buffer = [0u8; 64 * 1024];
        let mut copied = 0;
        loop {
            self.check()?;
            let read = input.read(&mut buffer)?;
            self.check()?;
            if read == 0 {
                return Ok(copied);
            }
            output.write_all(&buffer[..read])?;
            copied += read as u64;
        }
    }
}

pub struct BudgetReader<R> {
    reader: R,
    budget: WorkBudget,
}

impl<R: Read> Read for BudgetReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.budget.check()?;
        let read = self.reader.read(buffer)?;
        self.budget.check()?;
        Ok(read)
    }
}

impl<R: Seek> Seek for BudgetReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.budget.check()?;
        let position = self.reader.seek(position)?;
        self.budget.check()?;
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_archive_reader_does_not_retry_read_exact_forever() {
        let budget = WorkBudget::default();
        drop(budget.cancel_on_drop());
        let reader_budget = budget.clone();
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reader = reader_budget.reader(std::io::Cursor::new(b"zip"));
            let result = reader.read_exact(&mut [0u8; 3]);
            completed_tx.send(result).expect("completion");
        });

        let result = completed_rx.recv_timeout(Duration::from_millis(200));
        budget.state.cancelled.store(false, Ordering::Release);
        worker.join().expect("reader worker");

        assert!(result.is_ok(), "read_exact must stop on cancellation instead of retrying it");
        assert!(result.expect("completed").is_err());
    }
}
