#!/usr/bin/env node
// demo.js - Demo of enough-ffi usage from Node.js
//
// This demonstrates both synchronous and async patterns for
// bridging JavaScript AbortSignal to Rust cooperative cancellation.

import {
    CancellationHandle,
    runWithCancellation,
    runWithCancellationAsync,
    runWithTimeout,
    nativeLib,
} from './enough.js';

/**
 * Mock "Rust library" - simulates what your actual FFI would do.
 * In reality, this would be a call to your Rust library through ffi-napi.
 */
const MockRustLibrary = {
    /**
     * Simulates a Rust function that does CPU-intensive work
     * while periodically checking for cancellation.
     */
    processData(input, tokenHandle) {
        const output = Buffer.alloc(input.length);

        for (let i = 0; i < input.length; i++) {
            // Rust code would call: stop.check()?;
            // We simulate by checking the token handle
            if (nativeLib.enough_token_is_cancelled(tokenHandle)) {
                const error = new Error('Operation cancelled by Rust');
                error.name = 'AbortError';
                throw error;
            }

            // Simulate work
            output[i] = input[i] ^ 0xFF;

            // Simulate slow processing with occasional yields
            if (i % 10000 === 0 && i > 0) {
                // In real FFI, this would be inside Rust
                // Here we simulate blocking work
            }
        }

        return output;
    },
};

async function main() {
    console.log('=== enough-ffi Node.js Demo ===\n');

    await demoSyncUsage();
    await demoAsyncUsage();
    await demoCancellation();
    await demoTimeout();
    await demoAbortController();

    console.log('\nAll demos completed!');
}

/**
 * Demo 1: Basic synchronous usage
 */
async function demoSyncUsage() {
    console.log('--- Demo 1: Synchronous Usage ---');

    const data = Buffer.alloc(50000);
    for (let i = 0; i < data.length; i++) {
        data[i] = Math.floor(Math.random() * 256);
    }

    // Simple sync call with no cancellation
    const result = runWithCancellation(
        (handle) => MockRustLibrary.processData(data, handle)
    );

    console.log(`Processed ${data.length} bytes -> ${result.length} bytes`);
    console.log();
}

/**
 * Demo 2: Async pattern
 */
async function demoAsyncUsage() {
    console.log('--- Demo 2: Async Pattern ---');

    const data = Buffer.alloc(100000);
    for (let i = 0; i < data.length; i++) {
        data[i] = Math.floor(Math.random() * 256);
    }

    // Run multiple operations "concurrently"
    const results = await Promise.all([
        runWithCancellationAsync((handle) => MockRustLibrary.processData(data, handle)),
        runWithCancellationAsync((handle) => MockRustLibrary.processData(data, handle)),
    ]);

    console.log(`Completed ${results.length} parallel operations`);
    console.log(`Result sizes: ${results[0].length}, ${results[1].length}`);
    console.log();
}

/**
 * Demo 3: Cancellation in action
 */
async function demoCancellation() {
    console.log('--- Demo 3: Cancellation ---');

    const data = Buffer.alloc(10_000_000); // Large data to ensure we can cancel
    for (let i = 0; i < data.length; i++) {
        data[i] = Math.floor(Math.random() * 256);
    }

    const controller = new AbortController();

    // Start operation
    const promise = runWithCancellationAsync(
        (handle) => MockRustLibrary.processData(data, handle),
        controller.signal
    );

    // Cancel after short delay
    setTimeout(() => controller.abort(), 5);

    try {
        await promise;
        console.log('Operation completed (was fast enough)');
    } catch (e) {
        if (e.name === 'AbortError') {
            console.log('Operation was cancelled successfully!');
        } else {
            throw e;
        }
    }

    console.log();
}

/**
 * Demo 4: Timeout pattern
 */
async function demoTimeout() {
    console.log('--- Demo 4: Timeout Pattern ---');

    const data = Buffer.alloc(100_000_000); // Very large to trigger timeout
    for (let i = 0; i < 1000; i++) {
        data[i] = Math.floor(Math.random() * 256);
    }

    try {
        const result = await runWithTimeout(
            (handle) => MockRustLibrary.processData(data, handle),
            50 // 50ms timeout
        );

        console.log(`Completed before timeout: ${result.length} bytes`);
    } catch (e) {
        if (e.name === 'AbortError') {
            console.log('Operation timed out as expected!');
        } else {
            throw e;
        }
    }

    console.log();
}

/**
 * Demo 5: Using AbortController directly
 */
async function demoAbortController() {
    console.log('--- Demo 5: AbortController Pattern ---');

    const data = Buffer.alloc(50000);
    for (let i = 0; i < data.length; i++) {
        data[i] = Math.floor(Math.random() * 256);
    }

    // Create an AbortController
    const controller = new AbortController();

    // Create handle bridged to the signal
    const handle = new CancellationHandle(controller.signal);

    // Listen for cancellation
    handle.on('cancelled', () => {
        console.log('Handle received cancellation event');
    });

    try {
        // This won't be cancelled since we don't abort
        const result = MockRustLibrary.processData(data, handle.handle);
        console.log(`Processed ${result.length} bytes without cancellation`);

        // Now demonstrate cancellation
        controller.abort();
        console.log(`After abort: isCancelled = ${handle.isCancelled}`);
    } finally {
        handle.dispose();
    }

    console.log();
}

main().catch(console.error);
