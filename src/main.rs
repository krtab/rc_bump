use std::{collections::HashMap, mem::align_of};

use rc_bump::{Paving, RcBumpMember};

struct GraphNode {
    tag: u64,
    neighbors: Vec<RcBumpMember<GraphNode>>,
}

fn main() {
    let mut nodes = HashMap::new();
    {
        let paving = Paving::new(100 * 32, align_of::<u64>());
        for i in 1_u64..=10_000 {
            let children = nodes
                .iter()
                .filter(|&(&tag, _)| i % tag == 0)
                .map(|x| x.1)
                .cloned()
                .collect();
            let node = GraphNode {
                tag: i,
                neighbors: children,
            };
            let node = paving.try_alloc_rc(node).ok().unwrap();
            nodes.insert(i, node);
        }
    }
    for (k, node) in &nodes {
        print!("{k}: ");
        for n in &node.neighbors {
            print!("{} ", n.tag)
        }
        println!();
    }
    let mut head = nodes[&10_000].clone();
    std::mem::drop(nodes);
    while let Some(new_head) = head.neighbors.last() {
        head = new_head.clone()
    }
}
