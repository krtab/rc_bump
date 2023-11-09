use std::{mem::align_of, mem::size_of, rc::Rc, time::Duration};


static DIVISORS : [(u32,[u32;64]) ; 10001] = 
    include!("divisors.txt")
;


fn get_divisors(n: u32) -> &'static [u32] {
    let (n_div, divs) = &DIVISORS[n as usize];
    &divs[..(*n_div as usize)]
}

use bumpalo::{collections::Vec as BumpVec, Bump};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rc_bump::{Paving, RcBumpMember};

struct GraphNodePaving {
    tag: u32,
    neighbors: Vec<RcBumpMember<GraphNodePaving>>,
}

fn generate_graph_paving(n: u32) {
    let mut nodes: Vec<RcBumpMember<GraphNodePaving>> = Vec::new();
    {
        let paving = Paving::new(100 * size_of::<GraphNodePaving>(), align_of::<GraphNodePaving>());
        for i in 1_u32..n {
            let children = get_divisors(i)
                .iter()
                .filter(|&&k|  i != k).map(|k| &nodes[*k as usize  - 1])
                .cloned()
                .collect();
            let node = GraphNodePaving {
                tag: i,
                neighbors: children,
            };
            let node = paving.try_alloc_rc(node).ok().unwrap();
            nodes.push(node);
        }
    }
    let mut head = nodes.pop().unwrap();
    std::mem::drop(nodes);
    while let Some(new_head) = head.neighbors.last() {
        head = new_head.clone()
    }
}

struct GraphNodeRc {
    tag: u32,
    neighbors: Vec<Rc<GraphNodeRc>>,
}

fn generate_graph_rc(n: u32) {
    let mut nodes: Vec<Rc<GraphNodeRc>> = Vec::new();
    {
        for i in 1_u32..n {
            let children = get_divisors(i)
                .iter()
                .filter(|&&k|  i != k).map(|k| &nodes[*k as usize  - 1])
                .cloned()
                .collect();
            let node = GraphNodeRc {
                tag: i,
                neighbors: children,
            };
            let node = Rc::new(node);
            nodes.push(node);
        }
    }
    let mut head = nodes.pop().unwrap();
    std::mem::drop(nodes);
    while let Some(new_head) = head.neighbors.last() {
        head = new_head.clone()
    }
}

struct GraphNodeBumpalo<'a> {
    tag: u32,
    neighbors: BumpVec<'a, &'a GraphNodeBumpalo<'a>>,
}

fn generate_graph_bumpalo(n: u32) {
    let bump = Bump::new();
    let mut nodes: Vec<&GraphNodeBumpalo> = Vec::new();
    {
        for i in 1_u32..n {
            let children = BumpVec::from_iter_in(
                get_divisors(i)
                .iter()
                .filter(|&&k|  i != k).map(|k| &nodes[*k as usize  - 1])
                .cloned(),
                &bump,
            );
            let node = GraphNodeBumpalo {
                tag: i,
                neighbors: children,
            };
            let node = bump.alloc(node);
            nodes.push(node);
        }
    }
    let mut head = nodes.pop().unwrap();
    std::mem::drop(nodes);
    while let Some(new_head) = head.neighbors.last() {
        head = new_head
    }
}

const BENCH_PARAMS: [u32; 7] = [10, 100, 64 * 3, 64 * 5, 64 * 3 * 5, 64 * 3 * 5 * 7,10_000];
pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("divisor_graph");
    group.warm_up_time(Duration::from_millis(1000));
    group.measurement_time(Duration::from_millis(5000));
    for n in BENCH_PARAMS {
        group.bench_with_input(BenchmarkId::new("Paving", n), &n, |b, &n| {
            b.iter(|| generate_graph_paving(n));
        });
        group.bench_with_input(BenchmarkId::new("Rc", n), &n, |b, &n| {
            b.iter(|| generate_graph_rc(n));
        });
        group.bench_with_input(BenchmarkId::new("Bumpalo", n), &n, |b, &n| {
            b.iter(|| generate_graph_bumpalo(n));
        });
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
