using System.Runtime.InteropServices;
using System.Text.Json;

namespace Sonago;

internal static class Native
{
    private const string Library = "sonago_core.dll";
    private static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower
    };

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void Wake();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "sonago_start")]
    private static extern nint StartCore(Wake wake);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "sonago_invoke")]
    private static extern nint InvokeCore([MarshalAs(UnmanagedType.LPUTF8Str)] string command);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "sonago_free")]
    private static extern void Free(nint reply);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, EntryPoint = "sonago_stop")]
    internal static extern void Stop();

    [DllImport("user32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int MessageBoxW(nint owner, string message, string caption, uint style);

    [DllImport("user32.dll", ExactSpelling = true)]
    internal static extern uint GetDpiForWindow(nint window);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern nint CreateRectRgn(int left, int top, int right, int bottom);

    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern bool DeleteObject(nint handle);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern int SetWindowRgn(nint window, nint region, bool redraw);

    internal static void ClipPanel(nint window, int width, int top, int bottom)
    {
        var region = CreateRectRgn(0, top, width, bottom);
        if (region == 0)
            return;
        // On success Windows takes ownership of the region.
        if (SetWindowRgn(window, region, true) == 0)
            DeleteObject(region);
    }

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint GetAncestor(nint window, uint flags);

    internal static bool OwnsForegroundWindow(nint window)
    {
        const uint rootOwner = 3;
        return GetAncestor(GetForegroundWindow(), rootOwner) == window;
    }

    [DllImport("dwmapi.dll", ExactSpelling = true)]
    private static extern int DwmSetWindowAttribute(nint window, uint attribute, in int value, uint size);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern int GetWindowLongW(nint window, int index);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern int SetWindowLongW(nint window, int index, int value);

    internal static void ConfigurePanelFrame(nint window)
    {
        const int windowStyle = -16;
        const int popup = unchecked((int)0x80000000);
        SetWindowLongW(window, windowStyle, GetWindowLongW(window, windowStyle) | popup);
        const uint cornerPreference = 33;
        const int doNotRound = 1; // WinUI draws the corners of the sliding surface.
        _ = DwmSetWindowAttribute(window, cornerPreference, doNotRound, sizeof(int));
        const uint borderColor = 34;
        const int noBorder = -2; // DWMWA_COLOR_NONE
        _ = DwmSetWindowAttribute(window, borderColor, noBorder, sizeof(int));
    }

    internal static void DisableWindowTransitions(nint window)
    {
        const uint TransitionsForceDisabled = 3;
        const int disabled = 1;
        _ = DwmSetWindowAttribute(window, TransitionsForceDisabled, disabled, sizeof(int));
    }

    internal static void ShowStartupError(string message)
    {
        const uint ErrorIcon = 0x10;
        MessageBoxW(0, message, "Sonago", ErrorIcon);
    }

    internal static bool? Start(Wake wake) => Decode<bool?>(StartCore(wake));
    internal static T Call<T>(object command) => Decode<T>(InvokeCore(JsonSerializer.Serialize(command, Json)));
    internal static void Run(object command) => Call<JsonElement>(command);

    private static T Decode<T>(nint pointer)
    {
        try
        {
            var text = Marshal.PtrToStringUTF8(pointer) ?? throw new InvalidOperationException("Sonago returned no response.");
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
internal sealed record Events(string[] Actions, string[] Clips);
internal sealed record Settings(Source[] Sources, int Retention, int Minutes, string Output,
    string Format, string Hotkey, string DoubleTap, bool Autostart, bool Notifications, int SaveSound, bool Transcribe);
internal sealed record DeviceChoice(string Label, string? Endpoint, bool Selected);
internal sealed record Preferences(Settings Settings, DeviceChoice[][] Devices, string[] SaveSoundChoices, string? Issue);
internal sealed record SaveLength(int Minutes, string Label, bool IsDefault);
internal sealed record Placement(int X, int Y, int Width, int Height, int Dpi, int WorkBottom);
