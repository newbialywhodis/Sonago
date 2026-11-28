using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI.ViewManagement;

namespace Sonago;

public sealed partial class PanelWindow
{
    private readonly UISettings uiSettings = new();
    private Placement? placement;
    private RectInt32 viewport;
    private PointInt32 outward;
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
            centered = setup
        });

        var bounds = new RectInt32(place.X, place.Y, place.Width, place.Height);
        bool samePlacement = placement == place;
        if (!newAnchor && samePlacement)
            return;
        bool reversing = slideFrom.HasValue && samePlacement;
        StopSlide();
        placement = place;
        panelScale = place.Dpi / 96d;
        if (setup)
        {
            Root.Padding = new Thickness(0);
            Native.ClearPanelClip(hwnd);
            AppWindow.MoveAndResize(bounds);
            return;
        }

        // Extend the HWND to the taskbar, then clip the moving card inside it.
        var work = place.WorkArea;
        outward = place.Outward;
        int gap = place.Edge switch
        {
            TaskbarEdge.Left => bounds.X - work.Left,
            TaskbarEdge.Top => bounds.Y - work.Top,
            TaskbarEdge.Right => work.Right - bounds.X - bounds.Width,
            _ => work.Bottom - bounds.Y - bounds.Height
        };
        travel = gap + (outward.X == 0 ? bounds.Height : bounds.Width);
        viewport = new RectInt32(
            bounds.X - (outward.X < 0 ? gap : 0),
            bounds.Y - (outward.Y < 0 ? gap : 0),
            bounds.Width + Math.Abs(outward.X) * gap,
            bounds.Height + Math.Abs(outward.Y) * gap);
        Root.Padding = new Thickness(
            (bounds.X - viewport.X) / panelScale, (bounds.Y - viewport.Y) / panelScale,
            (viewport.X + viewport.Width - bounds.X - bounds.Width) / panelScale,
            (viewport.Y + viewport.Height - bounds.Y - bounds.Height) / panelScale);
        AppWindow.MoveAndResize(viewport);

        SaveMenu.Placement = MicrophoneMenu.Placement = SystemMenu.Placement = place.Edge switch
        {
            TaskbarEdge.Left => FlyoutPlacementMode.Right,
            TaskbarEdge.Top => FlyoutPlacementMode.Bottom,
            TaskbarEdge.Right => FlyoutPlacementMode.Left,
            _ => FlyoutPlacementMode.Top
        };
        if (SettingsPopup.IsOpen)
            LayoutSettingsPopup();
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
        var place = placement!;
        offset = pixels;
        int dx = outward.X * pixels;
        int dy = outward.Y * pixels;
        PanelOffset.X = dx / panelScale;
        PanelOffset.Y = dy / panelScale;
        int x = place.X - viewport.X + dx;
        int y = place.Y - viewport.Y + dy;
        compactBackdrop?.MoveTo(x, y);
        Native.ClipPanel(hwnd, Math.Max(0, x), Math.Max(0, y),
            Math.Min(viewport.Width, x + place.Width), Math.Min(viewport.Height, y + place.Height));
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
