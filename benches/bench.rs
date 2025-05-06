use blockstore_ffi::store::Store;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use pprof::criterion::Output::Flamegraph;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

fn bench_write_block(c: &mut Criterion) {
    let tmpdir = tempfile::tempdir().unwrap();
    let store = Store::new(
        tmpdir.path(),
        tmpdir.path(),
        NonZeroUsize::new(1024).unwrap(),
        true,
        true,
        1,
    )
    .unwrap();
    let block = vec![32; 1024];
    let mut block_height = 1;

    #[allow(clippy::arithmetic_side_effects)]
    c.bench_function("write_block", |b| {
        b.iter(|| {
            for _ in 0..256 {
                store
                    .write_block(black_box(block_height), black_box(&block))
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
        true,
        1,
    )
    .unwrap();
    let block = vec![32; 1024];
    store.write_block(1, &block).unwrap();

    c.bench_function("read_block", |b| {
        b.iter(|| {
            store.read_block(black_box(1)).unwrap();
        });
    });
}

fn bench_write_block_parallel(c: &mut Criterion) {
    use std::thread;

    #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let threads = thread::available_parallelism()
        .unwrap_or(NonZeroUsize::new(1).unwrap())
        .get() as i32;
    let height = AtomicU64::new(1);
    let block = vec![32; 1024];

    c.bench_function("write_block_parallel", |b| {
        b.iter_batched(
            || {
                let tmpdir = tempfile::tempdir().unwrap();
                Store::new(
                    tmpdir.path(),
                    tmpdir.path(),
                    NonZeroUsize::new(1024).unwrap(),
                    true,
                    true,
                    1,
                )
                .unwrap()
            },
            |store| {
                thread::scope(|s| {
                    for _cache_size in 1..=threads {
                        s.spawn(|| {
                            for _ in 0..256 {
                                let height = height.fetch_add(1, Ordering::Relaxed);
                                store
                                    .write_block(height, &block)
                                    .unwrap_or_else(|_| panic!("Block height {height}"));
                            }
                        });
                    }
                });
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group! {
    name = linear;
    config = Criterion::default().sample_size(10).with_profiler(pprof::criterion::PProfProfiler::new(100, Flamegraph(None)));
    targets = bench_write_block, bench_read_block
}
criterion_group! {
    name = parallel;
    config = Criterion::default().sample_size(10).with_profiler(pprof::criterion::PProfProfiler::new(100, Flamegraph(None)));
    targets = bench_write_block_parallel
}
criterion_main!(linear, parallel);
