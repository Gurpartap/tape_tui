use std::hint::black_box;
use std::time::{Duration, Instant};

use tape_tui::core::component::Component;
use tape_tui::{Markdown, MarkdownTheme};

const WIDTH: usize = 110;
const WARMUP_ITERS: usize = 30;
const MEASURE_ITERS: usize = 300;

#[derive(Debug, Clone, Copy)]
struct Stats {
    mean_ns: f64,
    p50_ns: u128,
    p95_ns: u128,
}

#[derive(Debug, Clone, Copy)]
struct BenchResult {
    cold_ns: u128,
    fresh_stats: Stats,
    fresh_checksum: usize,
    reuse_stats: Stats,
    reuse_checksum: usize,
}

fn plain(text: &str) -> String {
    text.to_string()
}

fn theme_with_highlighting_on() -> MarkdownTheme {
    MarkdownTheme {
        heading: Box::new(plain),
        link: Box::new(plain),
        link_url: Box::new(plain),
        code: Box::new(plain),
        code_block: Box::new(plain),
        code_block_border: Box::new(plain),
        quote: Box::new(plain),
        quote_border: Box::new(plain),
        hr: Box::new(plain),
        list_bullet: Box::new(plain),
        bold: Box::new(plain),
        italic: Box::new(plain),
        strikethrough: Box::new(plain),
        underline: Box::new(plain),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn theme_with_highlighting_off() -> MarkdownTheme {
    MarkdownTheme {
        heading: Box::new(plain),
        link: Box::new(plain),
        link_url: Box::new(plain),
        code: Box::new(plain),
        code_block: Box::new(plain),
        code_block_border: Box::new(plain),
        quote: Box::new(plain),
        quote_border: Box::new(plain),
        hr: Box::new(plain),
        list_bullet: Box::new(plain),
        bold: Box::new(plain),
        italic: Box::new(plain),
        strikethrough: Box::new(plain),
        underline: Box::new(plain),
        highlight_code: Some(Box::new(|code, _| {
            code.split('\n').map(|line| line.to_string()).collect()
        })),
        code_block_indent: None,
    }
}

fn nanos(duration: Duration) -> u128 {
    duration.as_nanos()
}

fn compute_stats(samples_ns: &[u128]) -> Stats {
    let mut sorted = samples_ns.to_vec();
    sorted.sort_unstable();

    let mean_ns =
        samples_ns.iter().map(|value| *value as f64).sum::<f64>() / samples_ns.len() as f64;
    let p50_ns = sorted[sorted.len() / 2];
    let mut p95_index = (sorted.len() * 95) / 100;
    if p95_index >= sorted.len() {
        p95_index = sorted.len() - 1;
    }
    let p95_ns = sorted[p95_index];

    Stats {
        mean_ns,
        p50_ns,
        p95_ns,
    }
}

fn benchmark_case(theme: fn() -> MarkdownTheme, sample: &str) -> BenchResult {
    let streaming_ends = streaming_end_offsets(sample, MEASURE_ITERS);

    // Cold render (captures one-time init cost where applicable).
    let cold_start = Instant::now();
    let mut cold_markdown = Markdown::new(sample, 0, 0, theme(), None);
    let cold_lines = cold_markdown.render(WIDTH);
    let cold_checksum_seed = cold_lines.len();
    black_box(cold_lines);
    let cold_ns = nanos(cold_start.elapsed());

    // Warm-up: fresh instance mode.
    for _ in 0..WARMUP_ITERS {
        let mut markdown = Markdown::new(sample, 0, 0, theme(), None);
        let lines = markdown.render(WIDTH);
        black_box(lines);
    }

    // Warm-up: reuse + set_text mode.
    let mut warm_markdown = Markdown::new("", 0, 0, theme(), None);
    for i in 0..WARMUP_ITERS {
        let end = streaming_ends[i % streaming_ends.len()];
        warm_markdown.set_text(&sample[..end]);
        let lines = warm_markdown.render(WIDTH);
        black_box(lines);
    }

    // Measured steady-state: fresh instance mode.
    let mut fresh_samples_ns = Vec::with_capacity(MEASURE_ITERS);
    let mut fresh_checksum = cold_checksum_seed;

    for _ in 0..MEASURE_ITERS {
        let start = Instant::now();
        let mut markdown = Markdown::new(sample, 0, 0, theme(), None);
        let lines = markdown.render(WIDTH);
        fresh_checksum ^= lines.len();
        black_box(lines);
        fresh_samples_ns.push(nanos(start.elapsed()));
    }

    // Measured steady-state: reuse + set_text mode (stream-like incremental text updates).
    let mut reuse_markdown = Markdown::new("", 0, 0, theme(), None);
    let mut reuse_samples_ns = Vec::with_capacity(MEASURE_ITERS);
    let mut reuse_checksum = cold_checksum_seed;
    for end in streaming_ends {
        reuse_markdown.set_text(&sample[..end]);
        let start = Instant::now();
        let lines = reuse_markdown.render(WIDTH);
        reuse_checksum ^= lines.len();
        black_box(lines);
        reuse_samples_ns.push(nanos(start.elapsed()));
    }

    BenchResult {
        cold_ns,
        fresh_stats: compute_stats(&fresh_samples_ns),
        fresh_checksum,
        reuse_stats: compute_stats(&reuse_samples_ns),
        reuse_checksum,
    }
}

fn streaming_end_offsets(sample: &str, count: usize) -> Vec<usize> {
    let mut boundaries: Vec<usize> = sample.char_indices().map(|(idx, _)| idx).collect();
    boundaries.push(sample.len());

    let total_steps = boundaries.len().saturating_sub(1);
    if total_steps == 0 {
        return vec![0; count.max(1)];
    }

    let mut ends = Vec::with_capacity(count.max(1));
    for i in 0..count.max(1) {
        let mut step = ((i + 1) * total_steps) / count.max(1);
        if step == 0 {
            step = 1;
        }
        ends.push(boundaries[step]);
    }
    ends
}

fn ns_to_ms(ns: u128) -> f64 {
    ns as f64 / 1_000_000.0
}

fn main() {
    let sample = sample_markdown();

    let off = benchmark_case(theme_with_highlighting_off, sample);
    let on = benchmark_case(theme_with_highlighting_on, sample);

    println!("markdown highlight benchmark");
    println!("width={WIDTH}, warmup_iters={WARMUP_ITERS}, measure_iters={MEASURE_ITERS}");
    println!("sample_blocks=10 (zig, c++, haskell, ocaml, lisp, c, go, rust, mermaid, dot)");
    println!();

    println!("[off: plain no-op highlighter]");
    println!("cold_ms={:.3}", ns_to_ms(off.cold_ns));
    println!(
        "fresh_mean_ms={:.3} fresh_p50_ms={:.3} fresh_p95_ms={:.3} fresh_checksum={}",
        off.fresh_stats.mean_ns / 1_000_000.0,
        off.fresh_stats.p50_ns as f64 / 1_000_000.0,
        off.fresh_stats.p95_ns as f64 / 1_000_000.0,
        off.fresh_checksum
    );
    println!(
        "reuse_mean_ms={:.3} reuse_p50_ms={:.3} reuse_p95_ms={:.3} reuse_checksum={}",
        off.reuse_stats.mean_ns / 1_000_000.0,
        off.reuse_stats.p50_ns as f64 / 1_000_000.0,
        off.reuse_stats.p95_ns as f64 / 1_000_000.0,
        off.reuse_checksum
    );
    println!();

    println!("[on: default tape_tui syntect highlighter]");
    println!("cold_ms={:.3}", ns_to_ms(on.cold_ns));
    println!(
        "fresh_mean_ms={:.3} fresh_p50_ms={:.3} fresh_p95_ms={:.3} fresh_checksum={}",
        on.fresh_stats.mean_ns / 1_000_000.0,
        on.fresh_stats.p50_ns as f64 / 1_000_000.0,
        on.fresh_stats.p95_ns as f64 / 1_000_000.0,
        on.fresh_checksum
    );
    println!(
        "reuse_mean_ms={:.3} reuse_p50_ms={:.3} reuse_p95_ms={:.3} reuse_checksum={}",
        on.reuse_stats.mean_ns / 1_000_000.0,
        on.reuse_stats.p50_ns as f64 / 1_000_000.0,
        on.reuse_stats.p95_ns as f64 / 1_000_000.0,
        on.reuse_checksum
    );
    println!();

    println!("[relative slowdown: on/off]");
    println!("cold_x={:.2}", on.cold_ns as f64 / off.cold_ns as f64,);
    println!(
        "fresh_mean_x={:.2} fresh_p50_x={:.2} fresh_p95_x={:.2}",
        on.fresh_stats.mean_ns / off.fresh_stats.mean_ns,
        on.fresh_stats.p50_ns as f64 / off.fresh_stats.p50_ns as f64,
        on.fresh_stats.p95_ns as f64 / off.fresh_stats.p95_ns as f64,
    );
    println!(
        "reuse_mean_x={:.2} reuse_p50_x={:.2} reuse_p95_x={:.2}",
        on.reuse_stats.mean_ns / off.reuse_stats.mean_ns,
        on.reuse_stats.p50_ns as f64 / off.reuse_stats.p50_ns as f64,
        on.reuse_stats.p95_ns as f64 / off.reuse_stats.p95_ns as f64,
    );
}

fn sample_markdown() -> &'static str {
    r#"
# Highlight benchmark corpus

```zig
const std = @import("std");

pub fn main() !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var list = std.ArrayList(i64).init(allocator);
    defer list.deinit();

    for (0..20) |i| {
        try list.append(@as(i64, @intCast(i * i)));
    }

    const sum = blk: {
        var acc: i64 = 0;
        for (list.items) |value| acc += value;
        break :blk acc;
    };

    std.debug.print("sum={d} len={d}\n", .{ sum, list.items.len });
}
```

```cpp
#include <algorithm>
#include <cstdint>
#include <iostream>
#include <numeric>
#include <string>
#include <vector>

struct Node {
  std::string name;
  std::vector<int> values;
};

int main() {
  Node n{"alpha", {1, 2, 3, 5, 8, 13}};
  auto total = std::accumulate(n.values.begin(), n.values.end(), 0LL);
  std::sort(n.values.begin(), n.values.end(), std::greater<int>());

  for (auto v : n.values) {
    std::cout << n.name << ":" << v << "\n";
  }

  std::cout << "total=" << total << std::endl;
}
```

```haskell
{-# LANGUAGE OverloadedStrings #-}

import Data.List (foldl')
import qualified Data.Map.Strict as M

step :: M.Map String Int -> String -> M.Map String Int
step acc token = M.insertWith (+) token 1 acc

main :: IO ()
main = do
  let tokens = words "lorem ipsum ipsum dolor sit amet amet amet"
      counts = foldl' step M.empty tokens
      score = sum (M.elems counts)
  print counts
  putStrLn ("score=" <> show score)
```

```ocaml
module SMap = Map.Make(String)

let count_words s =
  let words = String.split_on_char ' ' s in
  List.fold_left
    (fun acc w ->
      let prev = Option.value (SMap.find_opt w acc) ~default:0 in
      SMap.add w (prev + 1) acc)
    SMap.empty
    words

let () =
  let counts = count_words "ocaml map fold fold map" in
  SMap.iter (fun k v -> Printf.printf "%s:%d\n" k v) counts
```

```lisp
(defpackage :bench
  (:use :cl))
(in-package :bench)

(defun fib (n)
  (labels ((iter (a b i)
             (if (= i 0)
                 a
                 (iter b (+ a b) (- i 1)))))
    (iter 0 1 n)))

(format t "fib(20)=~a~%" (fib 20))
```

```c
#include <stdint.h>
#include <stdio.h>

typedef struct {
  const char *name;
  uint32_t count;
} item_t;

static uint64_t checksum(const item_t *items, size_t len) {
  uint64_t acc = 1469598103934665603ULL;
  for (size_t i = 0; i < len; ++i) {
    acc ^= items[i].count;
    acc *= 1099511628211ULL;
  }
  return acc;
}

int main(void) {
  item_t items[] = {{"a", 1}, {"b", 2}, {"c", 3}, {"d", 5}};
  printf("checksum=%llu\n", (unsigned long long)checksum(items, 4));
  return 0;
}
```

```go
package main

import (
  "fmt"
  "slices"
)

type User struct {
  Name  string
  Score int
}

func main() {
  users := []User{{"a", 2}, {"b", 5}, {"c", 3}}
  slices.SortFunc(users, func(x, y User) int { return y.Score - x.Score })

  total := 0
  for _, u := range users {
    total += u.Score
    fmt.Printf("%s:%d\n", u.Name, u.Score)
  }
  fmt.Printf("total=%d\n", total)
}
```

```rust
use std::collections::BTreeMap;

fn histogram(input: &str) -> BTreeMap<char, usize> {
    let mut map = BTreeMap::new();
    for ch in input.chars().filter(|c| c.is_ascii_alphabetic()) {
        *map.entry(ch.to_ascii_lowercase()).or_insert(0) += 1;
    }
    map
}

fn main() {
    let hist = histogram("Ferris says hello from rust rust rust");
    for (ch, count) in hist {
        println!("{ch}: {count}");
    }
}
```

```mermaid
flowchart TD
    Start([Start]) --> Parse{Parse markdown}
    Parse -->|code fence| Normalize[Normalize language]
    Normalize --> Highlight[Highlight code]
    Highlight --> Reset[Append ANSI reset]
    Reset --> End([Render lines])
    Parse -->|non-code| End
```

```dot
digraph RenderFlow {
  rankdir=LR;
  node [shape=box, style=rounded];
  input -> markdown_parser;
  markdown_parser -> code_block [label="fence"];
  code_block -> language_normalizer;
  language_normalizer -> syntax_lookup;
  syntax_lookup -> highlighted [label="known"];
  syntax_lookup -> plain [label="unknown"];
  highlighted -> ansi_reset;
  ansi_reset -> output;
  plain -> output;
}
```
"#
}
