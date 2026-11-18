use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub fn map<T: Sync, R: Send>(items: &[T], limit: usize, work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let workers = limit.min(items.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else { break };
                let result = work(item);
                if let Ok(mut all) = results.lock() {
                    all.push((index, result));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_default();
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}
