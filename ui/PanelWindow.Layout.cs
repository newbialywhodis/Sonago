using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI.ViewManagement;

namespace Sonago;

public sealed partial class PanelWindow
{
    private readonly UISettings uiSettings = new();
    private RectInt32 restingBounds;
    private int travel;
    private double panelScale;
    private int offset;
    private int? slideFrom;
    private long slideStart;

    private void Resize(bool newAnchor = false, bool slideIn = false)
    {
        var outer = AppWindow.Size;
        var client = AppWindow.ClientSize;
        double currentScale = WindowDpi() / 96d;
        double frameWidth = Math.Max(0, outer.Width - client.Width) / currentScale;
        double frameHeight = Math.Max(0, outer.Height - client.Height) / currentScale;

        int width = setup ? SettingsWidth : PanelWidth;
        QuickPanel.Measure(new Size(Math.Max(1, width - frameWidth), double.PositiveInfinity));
        double contentHeight = setup ? SetupHeight : QuickPanel.DesiredSize.Height;
        var place = Native.Call<Placement>(new
        {
            action = "place",
            width,
            height = (int)Math.Ceiling(contentHeight + frameHeight),
            centered = setup,
            newAnchor
        });

        var bounds = new RectInt32(place.X, place.Y, place.Width, place.Height);
        int distance = place.WorkBottom - place.Y;
        bool sameBounds = restingBounds.Equals(bounds) && travel == distance && panelScale == place.Dpi / 96d;
        if (!newAnchor && sameBounds)
            return;
        bool reversing = slideFrom.HasValue && sameBounds;
        StopSlide();
        restingBounds = bounds;
        travel = distance;
        panelScale = place.Dpi / 96d;
        // The HWND itself ends at the work area. Only its contents travel towards the taskbar.
        if (!setup)
            bounds.Height = travel;
        AppWindow.MoveAndResize(bounds);
        if (setup)
            return;
        bool animate = slideIn && uiSettings.AnimationsEnabled;
        SetOffset(animate ? (reversing ? offset : travel - 1) : 0);
        if (animate)
            StartSlide(offset);
    }

    private void SlideOut()
    {
        if (QuickPanel.Visibility != Visibility.Visible || !uiSettings.AnimationsEnabled)
        {
            FinishSlide();
            return;
        }
        StartSlide(offset);
    }

    private void StartSlide(int from)
    {
        if (!slideFrom.HasValue)
            CompositionTarget.Rendering += AdvanceSlide;
        slideFrom = from;
        slideStart = 0;
    }

    private void AdvanceSlide(object? sender, object args)
    {
        if (slideFrom is not int from)
            return;
        if (slideStart == 0)
            slideStart = Stopwatch.GetTimestamp();
        double progress = Stopwatch.GetElapsedTime(slideStart).TotalMilliseconds / (shown ? 200 : 150);
        if (progress >= 1)
        {
            FinishSlide();
            return;
        }

        double remaining = 1 - progress;
        double eased = shown ? 1 - remaining * remaining * remaining : progress * progress;
        int to = shown ? 0 : travel - 1;
        int next = from + (int)Math.Round((to - from) * eased);
        if (next != offset)
            SetOffset(next);
    }

    private void SetOffset(int pixels)
    {
        offset = pixels;
        PanelOffset.Y = pixels / panelScale;
        Native.ClipPanel(hwnd, restingBounds.Width, pixels, Math.Min(pixels + restingBounds.Height, travel));
    }

    private void FinishSlide()
    {
        StopSlide();
        if (!shown)
            AppWindow.Hide();
        else
            SetOffset(0);
    }

    private void StopSlide()
    {
        CompositionTarget.Rendering -= AdvanceSlide;
        slideFrom = null;
    }

    private uint WindowDpi()
    {
        uint dpi = Native.GetDpiForWindow(hwnd);
        return dpi == 0 ? 96 : dpi;
    }
}
