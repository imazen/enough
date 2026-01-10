// enough.js - Node.js bindings for enough-ffi
//
// This demonstrates how to use Rust cooperative cancellation from Node.js
// using the ffi-napi library.

import ffi from 'ffi-napi';
import ref from 'ref-napi';
import { EventEmitter } from 'events';
import path from 'path';
import { fileURLToPath } from 'url';

// Get library path relative to this file
const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Configure library path - update this for your system
const LIBRARY_PATH = process.env.ENOUGH_LIB_PATH ||
    path.join(__dirname, '../../target/release/libenough_ffi');

// Define types
const voidPtr = ref.refType(ref.types.void);

// Load the native library
let lib;
try {
    lib = ffi.Library(LIBRARY_PATH, {
        'enough_cancellation_create': [voidPtr, []],
        'enough_cancellation_cancel': ['void', [voidPtr]],
        'enough_cancellation_is_cancelled': ['bool', [voidPtr]],
        'enough_cancellation_destroy': ['void', [voidPtr]],
    });
} catch (e) {
    console.error('Failed to load enough_ffi library:', e.message);
    console.error('Set ENOUGH_LIB_PATH environment variable to the library location');
    console.error('Build the library first: cargo build --release -p enough-ffi');
    process.exit(1);
}

/**
 * A handle to a Rust cancellation source.
 *
 * This class provides a way to create cancellation tokens that can be
 * passed to Rust FFI functions and cancelled from JavaScript.
 */
export class CancellationHandle extends EventEmitter {
    #handle;
    #disposed = false;
    #abortController;
    #abortListener;

    /**
     * Create a new cancellation handle.
     * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
     */
    constructor(signal) {
        super();
        this.#handle = lib.enough_cancellation_create();

        if (this.#handle.isNull()) {
            throw new Error('Failed to create cancellation handle');
        }

        // Bridge AbortSignal if provided
        if (signal) {
            this.#abortListener = () => {
                this.cancel();
            };
            signal.addEventListener('abort', this.#abortListener);

            // If already aborted, cancel immediately
            if (signal.aborted) {
                this.cancel();
            }
        }
    }

    /**
     * Create a cancellation handle with a timeout.
     * @param {number} timeoutMs - Timeout in milliseconds
     * @returns {CancellationHandle}
     */
    static withTimeout(timeoutMs) {
        const controller = new AbortController();
        const handle = new CancellationHandle(controller.signal);
        handle.#abortController = controller;

        setTimeout(() => {
            if (!handle.#disposed) {
                controller.abort();
            }
        }, timeoutMs);

        return handle;
    }

    /**
     * Get the raw pointer to pass to Rust FFI functions.
     * @returns {Buffer}
     */
    get handle() {
        if (this.#disposed) {
            throw new Error('CancellationHandle has been disposed');
        }
        return this.#handle;
    }

    /**
     * Check if cancellation has been requested.
     * @returns {boolean}
     */
    get isCancelled() {
        if (this.#handle.isNull()) return false;
        return lib.enough_cancellation_is_cancelled(this.#handle);
    }

    /**
     * Signal cancellation.
     */
    cancel() {
        if (this.#handle.isNull()) return;
        lib.enough_cancellation_cancel(this.#handle);
        this.emit('cancelled');
    }

    /**
     * Dispose of the handle and free Rust resources.
     */
    dispose() {
        if (this.#disposed) return;
        this.#disposed = true;

        // Remove abort listener if we have one
        if (this.#abortListener && this.#abortController) {
            // Can't easily remove listener, but that's ok since we're disposing
        }

        if (!this.#handle.isNull()) {
            lib.enough_cancellation_destroy(this.#handle);
            this.#handle = ref.NULL;
        }
    }

    /**
     * Ensure handle is disposed when garbage collected.
     */
    [Symbol.dispose]() {
        this.dispose();
    }
}

/**
 * Run a synchronous operation with cancellation support.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation handle
 * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
 * @returns {T}
 */
export function runWithCancellation(operation, signal) {
    const handle = new CancellationHandle(signal);
    try {
        if (signal?.aborted) {
            throw new DOMException('Operation was aborted', 'AbortError');
        }
        return operation(handle.handle);
    } finally {
        handle.dispose();
    }
}

/**
 * Run an async operation with cancellation support.
 * This wraps a synchronous FFI call to run on the thread pool.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation handle
 * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
 * @returns {Promise<T>}
 */
export async function runWithCancellationAsync(operation, signal) {
    const handle = new CancellationHandle(signal);

    try {
        if (signal?.aborted) {
            throw new DOMException('Operation was aborted', 'AbortError');
        }

        // Use setImmediate to allow the event loop to process cancellation
        return await new Promise((resolve, reject) => {
            // Check periodically for cancellation
            const checkInterval = setInterval(() => {
                if (handle.isCancelled) {
                    clearInterval(checkInterval);
                    reject(new DOMException('Operation was cancelled', 'AbortError'));
                }
            }, 10);

            // Run operation
            setImmediate(() => {
                try {
                    const result = operation(handle.handle);
                    clearInterval(checkInterval);
                    resolve(result);
                } catch (e) {
                    clearInterval(checkInterval);
                    reject(e);
                }
            });
        });
    } finally {
        handle.dispose();
    }
}

/**
 * Run an operation with a timeout.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation handle
 * @param {number} timeoutMs - Timeout in milliseconds
 * @returns {Promise<T>}
 */
export async function runWithTimeout(operation, timeoutMs) {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

    try {
        return await runWithCancellationAsync(operation, controller.signal);
    } finally {
        clearTimeout(timeoutId);
    }
}

// Export the raw library for direct access if needed
export { lib as nativeLib };
