//! A tiny inventory, to look at how source code is highlighted.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Item<'a> {
    name: &'a str,
    qty: u32,
    price: f64,
}

impl<'a> Item<'a> {
    /// Total value of the stock, with a 21% tax when `taxed` is set.
    pub fn value(&self, taxed: bool) -> f64 {
        let base = self.qty as f64 * self.price;
        if taxed { base * 1.21 } else { base }
    }
}

fn main() {
    let mut stock: HashMap<&str, Item> = HashMap::new();
    stock.insert("bolt", Item { name: "bolt", qty: 120, price: 0.35 });
    for (key, item) in &stock {
        println!("{key}: {:.2}\t{}", item.value(true), item.name); // unicode: café — 日本語
    }
    /* a block comment
       over two lines */
    let very_long_line = "this string is deliberately long so that the narrow width has to chop it somewhere";
    assert!(!very_long_line.is_empty());
}
