use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_placeholder(c: &mut Criterion) {
    // TODO: OKF パーサ実装後、benches/data/ の実データで計測に置き換える
    c.bench_function("version", |b| {
        b.iter(|| black_box(okfast::OKF_VERSION).len())
    });
}

criterion_group!(benches, bench_placeholder);
criterion_main!(benches);
