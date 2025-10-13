// Trie-based search index for O(k) lookups instead of O(n)
// Pre-built index for instant search results

use ahash::AHashMap;

pub struct SearchIndex {
    // Prefix trie for fast starts_with queries
    trie: TrieNode,
    // Inverted index for contains queries
    ngram_index: AHashMap<String, Vec<usize>>,
}

struct TrieNode {
    children: AHashMap<char, Box<TrieNode>>,
    package_indices: Vec<usize>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            children: AHashMap::new(),
            package_indices: Vec::new(),
        }
    }
}

impl SearchIndex {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            trie: TrieNode::new(),
            ngram_index: AHashMap::new(),
        }
    }
    
    #[allow(dead_code)]
    pub fn build(&mut self, package_names: &[(usize, String)]) {
        // Build trie for prefix matching
        for (idx, name) in package_names {
            let name_lower = name.to_lowercase();
            self.insert_trie(&name_lower, *idx);
            
            // Build n-gram index for contains matching
            self.build_ngrams(&name_lower, *idx);
        }
    }
    
    fn insert_trie(&mut self, name: &str, idx: usize) {
        let mut node = &mut self.trie;
        
        for ch in name.chars() {
            node = node.children.entry(ch).or_insert_with(|| Box::new(TrieNode::new()));
        }
        
        node.package_indices.push(idx);
    }
    
    fn build_ngrams(&mut self, name: &str, idx: usize) {
        // Build 3-grams for fast substring matching
        let chars: Vec<char> = name.chars().collect();
        
        for i in 0..chars.len().saturating_sub(2) {
            let ngram: String = chars[i..i + 3].iter().collect();
            self.ngram_index.entry(ngram).or_insert_with(Vec::new).push(idx);
        }
        
        // Also index 2-grams for short queries
        for i in 0..chars.len().saturating_sub(1) {
            let ngram: String = chars[i..i + 2].iter().collect();
            self.ngram_index.entry(ngram).or_insert_with(Vec::new).push(idx);
        }
    }
    
    #[allow(dead_code)]
    pub fn search_prefix(&self, prefix: &str) -> Vec<usize> {
        let prefix_lower = prefix.to_lowercase();
        let mut node = &self.trie;
        
        // Navigate to prefix node
        for ch in prefix_lower.chars() {
            match node.children.get(&ch) {
                Some(child) => node = child,
                None => return Vec::new(),
            }
        }
        
        // Collect all indices under this prefix
        let mut results = Vec::new();
        self.collect_indices(node, &mut results);
        results
    }
    
    #[allow(dead_code)]
    fn collect_indices(&self, node: &TrieNode, results: &mut Vec<usize>) {
        results.extend(&node.package_indices);
        
        for child in node.children.values() {
            self.collect_indices(child, results);
        }
    }
    
    #[allow(dead_code)]
    pub fn search_contains(&self, query: &str) -> Vec<usize> {
        let query_lower = query.to_lowercase();
        
        if query_lower.len() < 2 {
            return Vec::new();
        }
        
        // Use n-gram index for fast contains search
        let chars: Vec<char> = query_lower.chars().collect();
        let ngram_len = if chars.len() >= 3 { 3 } else { 2 };
        
        if chars.len() < ngram_len {
            return Vec::new();
        }
        
        let first_ngram: String = chars[0..ngram_len].iter().collect();
        
        if let Some(candidates) = self.ngram_index.get(&first_ngram) {
            candidates.clone()
        } else {
            Vec::new()
        }
    }
}
