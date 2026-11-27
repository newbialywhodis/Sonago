using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;

namespace Sonago;

// Accessed on the WinUI dispatcher. Closing stdin lets the worker finish
// every clip it has received, even after the tray app exits.
internal sealed class Transcripts
{
    private Process? worker;
    private readonly Queue<string> pending = new();

    internal void Enqueue(string clip, Action<string, string?> report)
    {
        Process? started = null;
        try
        {
            if (worker is null)
            {
                var start = new ProcessStartInfo(Path.Combine(AppContext.BaseDirectory, "transcription", "sonago-transcribe.exe"))
                {
                    UseShellExecute = false,
                    CreateNoWindow = true,
                    RedirectStandardInput = true,
                    RedirectStandardOutput = true,
                    StandardInputEncoding = new UTF8Encoding(false),
                    StandardOutputEncoding = Encoding.UTF8
                };
                start.ArgumentList.Add("--queue");
                worker = Process.Start(start) ?? throw new IOException("Could not start the transcript worker.");
                started = worker;
            }
            worker.StandardInput.WriteLine(clip);
            worker.StandardInput.Flush();
            pending.Enqueue(clip);
        }
        catch (Exception error) { report(clip, error.Message); }
        finally { if (started is not null) _ = ReadResults(started, report); }
    }

    private async Task ReadResults(Process process, Action<string, string?> report)
    {
        string failure = "The transcript worker stopped unexpectedly.";
        try
        {
            while (await process.StandardOutput.ReadLineAsync() is string line)
            {
                var error = JsonSerializer.Deserialize<string?>(line);
                report(pending.Dequeue(), error);
                if (pending.Count == 0) { Close(); break; }
            }
        }
        catch (Exception error) { failure = error.Message; }
        finally
        {
            if (worker == process)
            {
                while (pending.TryDequeue(out string? clip)) report(clip, failure);
                Close();
            }
            process.Dispose();
        }
    }

    internal void Close()
    {
        var process = worker;
        worker = null;
        try { process?.StandardInput.Close(); }
        catch (IOException) { } // A crashed worker has already closed its end.
    }
}
