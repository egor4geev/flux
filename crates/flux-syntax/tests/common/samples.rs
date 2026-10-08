//! Code samples for each language: free of syntax errors, with Cyrillic, emoji, and CRLF lines. In
//! the sources, `⏎` at the end of a line stands for `\r\n`, and `⇥` for a tab.

const RUST: &str = r##"//! Модуль 🦀⏎
use std::collections::HashMap;

/// Документация: «кавычки» и эмодзи 👍🏽
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

impl<T: Copy> Point<T> {⏎
    pub fn new(x: T, y: T) -> Self {⏎
        Self { x, y }⏎
    }⏎
}⏎

fn main() {
    let mut map = HashMap::new();
    map.insert("ключ", 'я');
    /* блочный
       комментарий */
    let raw = r#"сырой "текст""#;
    for (i, c) in "строка".chars().enumerate() {
        println!("{i}: {c} {}", raw.len());
    }
    match map.get("ключ") {
        Some(v) => drop(v),
        None => {}
    }
}
"##;

const PYTHON: &str = r#"# -*- coding: utf-8 -*-
import os


class Привет:
    """Документация 👍🏽"""

    def __init__(self, name):
        self.name = name

    def greet(self):
        if self.name:
            return f"Привет, {self.name}!"⏎
        else:⏎
            return None⏎


for i in range(10):
    print(i, os.getcwd())
"#;

const BASH: &str = r#"#!/usr/bin/env bash
set -euo pipefail

greet() {
  local name="${1:-мир}"
  echo "Привет, $name 👋"
}

cat <<EOF
heredoc строка $HOME
EOF

cat <<-'END'
⇥с отступом
⇥END

for f in *.rs; do
  if [[ -f "$f" ]]; then greet "$f" | wc -l; fi
done
"#;

const YAML: &str = r#"# конфигурация
name: flux
version: 0.1
tags: [редактор, rust, "🦀"]
build:
  steps:
    - run: cargo build
    - run: |
        cargo test
        echo готово
  env: {RUST_LOG: debug, CI: true}
anchors:
  base: &base
    a: 1
  derived:
    <<: *base
    b: null
"#;

const MARKDOWN: &str = r#"# Заголовок 🦀

Абзац с *выделением* и `кодом`.⏎
⏎
- пункт один
- пункт два
  продолжение

1. первый
2. второй

> цитата
> ещё

```rust
fn main() {}
```

Подзаголовок
------------

[ссылка](https://example.com "title")
"#;

const JSON: &str = r#"{
  "name": "flux",
  "версия": 1.5e3,
  "emoji": "👍🏽",
  "list": [1, 2, {"nested": true}, null],⏎
  "escape": "a\nb\u0041"
}
"#;

const TOML: &str = r#"# Cargo.toml
[package]
name = "flux"
version = "0.1.0"
"описание" = "редактор 🦀"

[dependencies]
ropey = { version = "1.6", features = ["simd"] }
a.b.c = 1979-05-27T07:32:00Z

[[bin]]
name = 'flux'
multi = """
строки
"""
"#;

const GO: &str = r#"package main

import (
⇥"fmt"
⇥"strings"
)

// Привет 🦀
type Point struct {
⇥X, Y int
}

func (p Point) Sum() int { return p.X + p.Y }

func main() {
⇥p := Point{X: 1, Y: 2}
⇥fmt.Println(strings.ToUpper("строка"), p.Sum(), len("ы"))
⇥for i := range 3 {
⇥⇥defer fmt.Println(i)
⇥}
}
"#;

const JAVASCRIPT: &str = r#"// Модуль 🦀
import { readFile } from 'fs';

const ПРИВЕТ = `шаблон ${1 + 2} строка`;

export async function load(path, { encoding = 'utf8' } = {}) {
  const text = await readFile(path, encoding);
  return text.split(/\r?\n/).map((line, i) => `${i}: ${line}`);⏎
}⏎

class Widget extends Base {
  static count = 0;
  render() { return <div className="x">{this.props.name}</div>; }
}
"#;

const TYPESCRIPT: &str = r#"// Типы 🦀
interface Point<T> {
  x: T;
  y?: T;
}

export function sum(a: number, b: number = 2): number {
  return a + b;
}

type Map = Record<string, Array<Point<number>>>;
enum Цвет { Красный = 'red', Синий = 'blue' }
const p: Point<number> = { x: 1 } as Point<number>;
"#;

const TSX: &str = r#"import React from 'react';

export function App({ title }: { title: string }) {
  const [n, setN] = React.useState<number>(0);
  return (
    <div className="app" onClick={() => setN(n + 1)}>
      <h1>{title} — {n} 👍🏽</h1>
    </div>
  );
}
"#;

pub fn sample(language: &str) -> String {
    let raw = match language {
        "rust" => RUST,
        "python" => PYTHON,
        "bash" => BASH,
        "yaml" => YAML,
        "markdown" => MARKDOWN,
        "json" => JSON,
        "toml" => TOML,
        "go" => GO,
        "javascript" => JAVASCRIPT,
        "typescript" => TYPESCRIPT,
        "tsx" => TSX,
        other => panic!("no sample for {other}"),
    };
    raw.replace("⏎\n", "\r\n").replace('⇥', "\t")
}

/// Insertions that make sense for the language: keywords, constructs, indentation.
pub fn snippets(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "fn f() {}",
            "let x = 1;",
            "\"строка\"",
            "// коммент",
            "impl X {",
            "r#\"",
            "'a",
            "::<T>",
            "/// док\n",
        ],
        "python" => &[
            "def f():\n    pass\n",
            "    ",
            "if x:\n",
            "\"\"\"",
            "return",
            "lambda: 0",
            "# коммент\n",
            "    else:\n",
        ],
        "bash" => &[
            "<<EOF\n",
            "EOF\n",
            "\tEOF\n",
            "<<-X\n",
            "$(",
            "${",
            "fi\n",
            "do\n",
            "echo \"ы\"\n",
            "\t",
        ],
        "yaml" => &[
            "- ",
            "key: value\n",
            "  ",
            "|\n",
            ">-\n",
            "&a ",
            "*a",
            "# коммент\n",
            "{",
            "---\n",
        ],
        "markdown" => &[
            "# ", "- ", "```\n", "> ", "1. ", "*", "`", "[a](b)", "\n---\n", "    ",
        ],
        "json" => &[
            "\"k\": 1,",
            "[]",
            "{}",
            "null",
            "true",
            "\"ключ\"",
            "\\u0041",
            "1e5",
        ],
        "toml" => &[
            "[t]\n",
            "k = 1\n",
            "\"\"\"",
            "[[a]]\n",
            "a.b = ",
            "'s'",
            "1979-05-27",
            "{ x = 1 }",
        ],
        "go" => &[
            "func f() {}\n",
            "x := 1\n",
            "\"ы\"",
            "`raw`",
            "// к\n",
            "if x {",
            "go ",
            "return\n",
        ],
        _ => &[
            "function f() {}",
            "const x = 1;",
            "`${",
            "<div>",
            "</div>",
            "=>",
            "/re/g",
            ": number",
            "<T>",
            "interface I {}",
        ],
    }
}
