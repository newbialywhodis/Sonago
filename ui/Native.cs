using System.Runtime.InteropServices;
using System.Text.Json;

namespace Hearlier;

internal static class Native
{
    private const string Library = "hearlier_core.dll";
    private static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower
    };

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void Wake();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "hearlier_start")]
    private static extern nint StartCore(Wake wake);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "hearlier_invoke")]
    private static extern nint InvokeCore([MarshalAs(UnmanagedType.LPUTF8Str)] string command);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "hearlier_free")]
    private static extern void Free(nint reply);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "hearlier_stop")]
    internal static extern void Stop();

    [DllImport("user32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int MessageBoxW(nint owner, string message, string caption, uint style);

    [DllImport("user32.dll", ExactSpelling = true)]
    internal static extern uint GetDpiForWindow(nint window);

    [DllImport("dwmapi.dll", ExactSpelling = true)]
    private static extern int DwmSetWindowAttribute(nint window, uint attribute, in int value, uint size);

    internal static void DisableWindowTransitions(nint window)
    {
        const uint TransitionsForceDisabled = 3;
        const int disabled = 1;
        _ = DwmSetWindowAttribute(window, TransitionsForceDisabled, disabled, sizeof(int));
    }

    internal static void ShowStartupError(string message)
    {
        const uint ErrorIcon = 0x10;
        MessageBoxW(0, message, "hearlier", ErrorIcon);
    }

    internal static bool? Start(Wake wake) => Decode<bool?>(StartCore(wake));
    internal static T Call<T>(object command) => Decode<T>(InvokeCore(JsonSerializer.Serialize(command, Json)));
    internal static void Run(object command) => Call<JsonElement>(command);

    private static T Decode<T>(nint pointer)
    {
        try
        {
            var text = Marshal.PtrToStringUTF8(pointer) ?? throw new InvalidOperationException("hearlier returned no response.");
            var reply = JsonSerializer.Deserialize<Reply>(text, Json)!;
            if (reply.Error is not null)
            {
                throw new InvalidOperationException(reply.Error);
            }
            return reply.Value.Deserialize<T>(Json)!;
        }
        finally
        {
            Free(pointer);
        }
    }

    private sealed record Reply(JsonElement Value, string? Error);
}

internal sealed record Snapshot(string? Warning, string? Issue, bool SystemOn, bool MicOn,
    bool CanSave, string SaveLabel);
internal sealed record Source(bool Enabled, string? Endpoint);
internal sealed record Settings(Source[] Sources, int Retention, int Minutes, string Output,
    string Format, string Hotkey, bool Autostart, bool Notifications);
internal sealed record DeviceChoice(string Label, string? Endpoint, bool Selected);
internal sealed record Preferences(Settings Settings, DeviceChoice[][] Devices, string? Issue);
internal sealed record SaveLength(int Minutes, string Label, bool IsDefault);
internal sealed record Placement(int X, int Y, int Width, int Height, int Dpi);
