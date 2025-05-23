use ethers::{
    types::{Address, U256},
    utils::keccak256,
};
use rand::{RngCore, thread_rng};
use rayon::prelude::*;
use std::{
    sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex},
    time::Instant,
    thread,
};

const MAX_NUMBER: U256 = U256::MAX;
const DEFAULT_BATCH_SIZE: usize = 2048;
const BATCHES_BEFORE_CHECK: usize = 2;

// Define a public struct for the mining result that will be used in lib.rs
pub struct MiningResult {
    pub duration: f64,
    pub gas_price: U256,
}

// This is the main function that lib.rs will call
pub async fn mine_gas_for_transaction(
    nonce: u64,
    gas: u64,
    from: Address,
    batch_size_option: Option<usize>,
    thread_count_option: Option<usize>,
) -> Result<MiningResult, Box<dyn std::error::Error>> {
    // Use the provided batch size or default
    let batch_size = batch_size_option.unwrap_or(DEFAULT_BATCH_SIZE);
    
    // Pre-compute values that don't change in the loop
    let mut nonce_bytes = [0u8; 32];
    U256::from(nonce).to_big_endian(&mut nonce_bytes);
    let nonce_hash = U256::from_big_endian(&keccak256(nonce_bytes));
    
    let address_hash = U256::from_big_endian(&keccak256(from.as_bytes()));
    let nonce_address_xor = nonce_hash ^ address_hash;
    
    let div_constant = MAX_NUMBER;
    
    let start = Instant::now();
    
    // Create a flag to signal when a solution is found
    let found = Arc::new(AtomicBool::new(false));
    
    // Use a channel to communicate results between threads
    let (tx, rx) = mpsc::channel();
    
    // Determine thread count
    let num_threads = thread_count_option.unwrap_or_else(num_cpus::get);
    println!("Using {} CPU threads for mining with batch size {}", num_threads, batch_size);
    
    // Pre-generate random candidates for all threads
    let candidates_pool = Arc::new(Mutex::new(
        (0..num_threads)
            .map(|_| {
                let mut batch = Vec::with_capacity(batch_size);
                for _ in 0..batch_size {
                    let mut candidate = [0u8; 32];
                    thread_rng().fill_bytes(&mut candidate);
                    batch.push(candidate);
                }
                batch
            })
            .collect::<Vec<_>>()
    ));
    
    // Create worker threads
    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let found = found.clone();
            let tx = tx.clone();
            let candidates_pool = candidates_pool.clone();
            let nonce_address_xor = nonce_address_xor;
            let gas_amount = gas;
            let div_constant = div_constant;
            
            thread::spawn(move || {
                let mut rng = thread_rng();
                let mut local_batch = Vec::with_capacity(batch_size);
                
                // Get initial batch of candidates
                {
                    let mut pool = candidates_pool.lock().unwrap();
                    if let Some(batch) = pool.get_mut(thread_id) {
                        local_batch = batch.clone();
                    } else {
                        // Create new batch if none exists
                        for _ in 0..batch_size {
                            let mut candidate = [0u8; 32];
                            rng.fill_bytes(&mut candidate);
                            local_batch.push(candidate);
                        }
                    }
                }
                
                // Continue mining until a solution is found
                'mining: while !found.load(Ordering::Relaxed) {
                    // Process batches before checking for solution
                    for _ in 0..BATCHES_BEFORE_CHECK {
                        // Modify some bytes in each candidate 
                        for candidate in &mut local_batch {
                            // Modify a quarter of the bytes
                            for i in 0..8 {
                                candidate[i] = rng.next_u32() as u8;
                            }
                            
                            let candidate_hash = U256::from_big_endian(&keccak256(&candidate));
                            let result_hash = nonce_address_xor ^ candidate_hash;
                            
                            // Check for division by zero
                            if result_hash == U256::zero() {
                                continue;
                            }
                            
                            let external_gas = div_constant / result_hash;
                            
                            if external_gas >= gas_amount.into() {
                                // We found a solution
                                found.store(true, Ordering::SeqCst);
                                let _ = tx.send(*candidate);
                                break 'mining;
                            }
                        }
                    }
                }
            })
        })
        .collect();
    
    // Use tokio to handle async/await
    let solution = tokio::task::spawn_blocking(move || {
        // Wait for a solution
        let result = rx.recv();
        
        // Signal all threads to stop
        found.store(true, Ordering::SeqCst);
        
        // Wait for all threads to finish
        for handle in handles {
            let _ = handle.join();
        }
        
        result
    }).await?;
    
    match solution {
        Ok(candidate) => {
            let duration = start.elapsed().as_secs_f64();
            let gas_price = U256::from_big_endian(&candidate);
            
            return Ok(MiningResult {
                duration,
                gas_price,
            });
        }
        Err(_) => {
            return Err("Mining failed - no solution found".into());
        }
    }
}