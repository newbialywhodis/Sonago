using System.Diagnostics;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI.ViewManagement;

namespace Hearlier;

public sealed partial class PanelWindow
{
    private const int SlideDistanceDip = 8;
    private const double SlideMilliseconds = 180;
    private readonly UISettings uiSettings = new();
    private RectInt32 restingBounds;
    private int slideOutY;
    private (int From, int To)? slide;
    private long slideStart;

    private void Resize(bool newAnchor = false, bool slideIn = false)
    {
        var position = AppWindow.Position;
        bool reversingExit = slide is { } current && current.From < current.To;
        StopSlide();
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
        bool sameBounds = restingBounds.Equals(bounds);
        restingBounds = bounds;
        int distance = (int)Math.Round(SlideDistanceDip * place.Dpi / 96d);
        slideOutY = bounds.Y + Math.Clamp(place.WorkBottom - (bounds.Y + bounds.Height), 0, distance);
        if (slideIn && uiSettings.AnimationsEnabled)
        {
            bounds.Y = reversingExit && sameBounds
                ? Math.Clamp(position.Y, bounds.Y, slideOutY)
                : slideOutY;
        }
        if (position.X != bounds.X || position.Y != bounds.Y || outer.Width != bounds.Width || outer.Height != bounds.Height)
        {
            AppWindow.MoveAndResize(bounds);
        }
        if (bounds.Y != restingBounds.Y)
            StartSlide(bounds.Y, restingBounds.Y);
    }

    private void SlideOut()
    {
        var position = AppWindow.Position;
        StopSlide();
        if (!setup && uiSettings.AnimationsEnabled && position.X == restingBounds.X &&
            position.Y >= restingBounds.Y && position.Y < slideOutY)
        {
            StartSlide(position.Y, slideOutY);
            return;
        }
        AppWindow.Hide();
    }

    private void StartSlide(int from, int to)
    {
        slide = (from, to);
        slideStart = Stopwatch.GetTimestamp();
        CompositionTarget.Rendering += AdvanceSlide;
    }

    private void AdvanceSlide(object? sender, object args)
    {
        if (slide is not { } current)
            return;

        double progress = Stopwatch.GetElapsedTime(slideStart).TotalMilliseconds / SlideMilliseconds;
        if (progress >= 1)
        {
            FinishSlide();
            return;
        }

        double remaining = 1 - progress;
        double eased = shown ? 1 - remaining * remaining * remaining : progress * progress;
        int y = current.From + (int)Math.Round((current.To - current.From) * eased);
        if (AppWindow.Position.Y != y)
            AppWindow.Move(new PointInt32(restingBounds.X, y));
    }

    private void FinishSlide()
    {
        if (slide is null)
            return;

        StopSlide();
        if (shown)
            AppWindow.Move(new PointInt32(restingBounds.X, restingBounds.Y));
        else
            AppWindow.Hide();
    }

    private void StopSlide()
    {
        if (slide is null)
            return;

        CompositionTarget.Rendering -= AdvanceSlide;
        slide = null;
    }

    private uint WindowDpi()
    {
        uint dpi = Native.GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        return dpi == 0 ? 96 : dpi;
    }
}
