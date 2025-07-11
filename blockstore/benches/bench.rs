use blockstore::{Store, SyncMode};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use pprof::criterion::Output::Flamegraph;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn bench_write_block_sync(c: &mut Criterion) {
    let tmpdir = tempfile::tempdir().unwrap();
    let store = Store::new(
        tmpdir.path(),
        tmpdir.path(),
        NonZeroUsize::new(1024).unwrap(),
        true,
        SyncMode::Sync,
        1,
    )
    .unwrap();
    let block = vec![32; 1024];
    let mut block_height = 1;

    #[allow(clippy::arithmetic_side_effects)]
    c.bench_function("write_256_sync", |b| {
        b.iter(|| {
            for _ in 0..256 {
                store
                    .write_block(black_box(block_height), black_box(&block), 0)
                    .unwrap();
                block_height += 1;
            }
        });
    });
}

fn bench_read_block(c: &mut Criterion) {
    let tmpdir = tempfile::tempdir().unwrap();
    let store = Store::new(
        tmpdir.path(),
        tmpdir.path(),
        NonZeroUsize::new(1024).unwrap(),
        true,
        SyncMode::Async,
        1,
    )
    .unwrap();
    let block = vec![32; 1024];
    store.write_block(1, &block, 0).unwrap();

    c.bench_function("read_async", |b| {
        b.iter(|| {
            store.read_block(black_box(1)).unwrap();
        });
    });
}

fn bench_write_block_parallel<const MODE: u8>(c: &mut Criterion) {
    use std::thread;

    let threads = thread::available_parallelism()
        .unwrap_or(NonZeroUsize::new(1).unwrap())
        .get();
    let height = AtomicU64::new(1);
    let block = vec![32; 1024];

    eprintln!("threads: {threads}");

    let mode = match MODE {
        0 => SyncMode::Async,
        1 => SyncMode::Sync,
        _ => unreachable!(),
    };

    let id = format!("write_parallel_{mode}_{threads}_threads");

    c.bench_function(&id, |b| {
        b.iter_custom(|iters| {
            let tmpdir = tempfile::tempdir().unwrap();
            let store = Store::new(
                tmpdir.path(),
                tmpdir.path(),
                NonZeroUsize::new(1024).unwrap(),
                true,
                mode.clone(),
                1,
            )
            .unwrap();
            let start = Instant::now();
            thread::scope(|s| {
                for _cache_size in 1..=threads {
                    s.spawn(|| {
                        #[allow(clippy::arithmetic_side_effects)]
                        for _ in 0..iters / threads as u64 {
                            let height = height.fetch_add(1, Ordering::Relaxed);
                            store
                                .write_block(height, &block, 0)
                                .unwrap_or_else(|_| panic!("Block height {height}"));
                        }
                    });
                }
            });
            start.elapsed()
        });
    });
}

// criterion_group! {
//     name = linear;
//     config = Criterion::default().sample_size(20).measurement_time(Duration::from_secs(30)).with_profiler(pprof::criterion::PProfProfiler::new(100, Flamegraph(None)));
//     targets = bench_write_block_sync, bench_read_block
// }
criterion_group! {
    name = asynchronous;
    config = Criterion::default().measurement_time(Duration::from_secs(10)).with_profiler(pprof::criterion::PProfProfiler::new(100, Flamegraph(None)));
    targets = bench_write_block_parallel<0>, bench_read_block
}
criterion_group! {
    name = synchronous;
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(10)).with_profiler(pprof::criterion::PProfProfiler::new(100, Flamegraph(None)));
    targets = bench_write_block_parallel<1>, bench_write_block_sync
}
criterion_main!(asynchronous, synchronous);
