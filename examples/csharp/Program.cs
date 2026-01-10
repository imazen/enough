// Program.cs - Demo of enough-ffi usage from C#
//
// This demonstrates both synchronous and async-async patterns for
// bridging .NET CancellationToken to Rust cooperative cancellation.

using System;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;
using Enough;

namespace EnoughDemo
{
    /// <summary>
    /// Mock native library methods - replace with your actual Rust library.
    /// These simulate what your Rust FFI functions would look like.
    /// </summary>
    internal static class MockRustLibrary
    {
        // In reality, these would be P/Invoke to your Rust library:
        //
        // [DllImport("mylib")]
        // public static extern int decode_image(
        //     byte[] data,
        //     int length,
        //     IntPtr cancellationHandle,
        //     out IntPtr result);

        /// <summary>
        /// Simulates a Rust function that does CPU-intensive work
        /// while periodically checking for cancellation.
        /// </summary>
        public static byte[] ProcessData(byte[] input, IntPtr cancelHandle)
        {
            var output = new byte[input.Length];

            for (int i = 0; i < input.Length; i++)
            {
                // Rust code would call: stop.check()?;
                // We simulate by checking the handle
                if (cancelHandle != IntPtr.Zero &&
                    NativeMethods.enough_cancellation_is_cancelled(cancelHandle))
                {
                    throw new OperationCanceledException("Operation cancelled by Rust");
                }

                // Simulate work
                output[i] = (byte)(input[i] ^ 0xFF);

                // Simulate slow processing
                if (i % 1000 == 0)
                {
                    Thread.Sleep(1);
                }
            }

            return output;
        }
    }

    class Program
    {
        static async Task Main(string[] args)
        {
            Console.WriteLine("=== enough-ffi C# Demo ===\n");

            await DemoSyncUsage();
            await DemoAsyncUsage();
            await DemoCancellation();
            await DemoTimeoutAsync();

            Console.WriteLine("\nAll demos completed!");
        }

        /// <summary>
        /// Demo 1: Basic synchronous usage
        /// </summary>
        static Task DemoSyncUsage()
        {
            Console.WriteLine("--- Demo 1: Synchronous Usage ---");

            var data = new byte[5000];
            new Random().NextBytes(data);

            // Simple sync call with no cancellation
            using var handle = new CancellationHandle();
            var result = MockRustLibrary.ProcessData(data, handle.Handle);

            Console.WriteLine($"Processed {data.Length} bytes -> {result.Length} bytes");
            Console.WriteLine();

            return Task.CompletedTask;
        }

        /// <summary>
        /// Demo 2: Async-async pattern - async C# calling blocking Rust
        /// </summary>
        static async Task DemoAsyncUsage()
        {
            Console.WriteLine("--- Demo 2: Async-Async Pattern ---");

            var data = new byte[10000];
            new Random().NextBytes(data);

            // Run multiple operations concurrently
            var task1 = CancellationExtensions.RunWithCancellationAsync(
                handle => MockRustLibrary.ProcessData(data, handle));

            var task2 = CancellationExtensions.RunWithCancellationAsync(
                handle => MockRustLibrary.ProcessData(data, handle));

            var results = await Task.WhenAll(task1, task2);

            Console.WriteLine($"Completed {results.Length} parallel operations");
            Console.WriteLine($"Result sizes: {results[0].Length}, {results[1].Length}");
            Console.WriteLine();
        }

        /// <summary>
        /// Demo 3: Cancellation in action
        /// </summary>
        static async Task DemoCancellation()
        {
            Console.WriteLine("--- Demo 3: Cancellation ---");

            var data = new byte[1_000_000]; // Large data to ensure we can cancel
            new Random().NextBytes(data);

            using var cts = new CancellationTokenSource();

            // Start operation
            var task = CancellationExtensions.RunWithCancellationAsync(
                handle => MockRustLibrary.ProcessData(data, handle),
                cts.Token);

            // Cancel after short delay
            await Task.Delay(50);
            cts.Cancel();

            try
            {
                await task;
                Console.WriteLine("Operation completed (was fast enough)");
            }
            catch (OperationCanceledException)
            {
                Console.WriteLine("Operation was cancelled successfully!");
            }

            Console.WriteLine();
        }

        /// <summary>
        /// Demo 4: Timeout using CancellationTokenSource
        /// </summary>
        static async Task DemoTimeoutAsync()
        {
            Console.WriteLine("--- Demo 4: Timeout Pattern ---");

            var data = new byte[10_000_000]; // Very large to trigger timeout
            new Random().NextBytes(data);

            // Create a timeout of 100ms
            using var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(100));

            try
            {
                var result = await CancellationExtensions.RunWithCancellationAsync(
                    handle => MockRustLibrary.ProcessData(data, handle),
                    cts.Token);

                Console.WriteLine($"Completed before timeout: {result.Length} bytes");
            }
            catch (OperationCanceledException)
            {
                Console.WriteLine("Operation timed out as expected!");
            }

            Console.WriteLine();
        }
    }
}
