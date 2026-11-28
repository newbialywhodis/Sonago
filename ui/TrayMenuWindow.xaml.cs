using System.Diagnostics;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI.ViewManagement;

namespace Sonago;

public sealed partial class TrayMenuWindow : Window
{
    private const int SlideDistanceDip = 12;
    private const double EntranceMilliseconds = 160;
    private const double ExitMilliseconds = 110;

    private readonly Action<bool> openPanel;
    private readonly Action requestExit;
    private readonly UISettings uiSettings = new();
    private RectInt32 restingBounds;
    private PointInt32 slideOffset;
    private (PointInt32 From, PointInt32 To)? slide;
    private long slideStart;
    private Action? afterDismiss;
    private bool shown;

    internal TrayMenuWindow(Action<bool> openPanel, Action requestExit)
    {
        this.openPanel = openPanel;
        this.requestExit = requestExit;
        InitializeComponent();

        var presenter = OverlappedPresenter.Create();
        presenter.SetBorderAndTitleBar(true, false);
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        Native.DisableWindowTransitions(WinRT.Interop.WindowNative.GetWindowHandle(this));

        Activated += (_, args) =>
        {
            if (args.WindowActivationState == WindowActivationState.Deactivated)
                Dismiss();
        };
        AppWindow.Closing += OnClosing;
        Root.Loaded += (_, _) =>
        {
            if (shown)
                OpenButton.Focus(FocusState.Programmatic);
        };
    }

    internal void ShowAtTray()
    {
        if (afterDismiss is not null)
            return;

        Root.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        var content = Root.DesiredSize;
        var frame = AppWindow.Size;
        var client = AppWindow.ClientSize;
        double scale = Math.Max(96u, Native.GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this))) / 96d;
        var place = Native.Call<Placement>(new
        {
            action = "place_tray_menu",
            width = (int)Math.Ceiling(content.Width + Math.Max(0, frame.Width - client.Width) / scale),
            height = (int)Math.Ceiling(content.Height + Math.Max(0, frame.Height - client.Height) / scale)
        });
        var bounds = new RectInt32(place.X, place.Y, place.Width, place.Height);
        var outward = place.Outward;
        int distance = (int)Math.Round(SlideDistanceDip * place.Dpi / 96d);
        var nextOffset = new PointInt32(outward.X * distance, outward.Y * distance);
        bool samePlacement = restingBounds.Equals(bounds) && slideOffset.Equals(nextOffset);
        if (shown && samePlacement)
            return;

        slideOffset = nextOffset;
        var rest = new PointInt32(bounds.X, bounds.Y);
        var initial = rest;
        if (uiSettings.AnimationsEnabled)
        {
            if (slide is not null && samePlacement)
                initial = AppWindow.Position;
            else if (!shown)
                initial = new PointInt32(rest.X + slideOffset.X, rest.Y + slideOffset.Y);
        }

        StopSlide();
        restingBounds = bounds;
        bounds.X = initial.X;
        bounds.Y = initial.Y;
        AppWindow.MoveAndResize(bounds);
        shown = true;
        Root.IsHitTestVisible = true;
        Activate();
        if (shown && !initial.Equals(rest))
            StartSlide(initial, rest);
        if (shown && Root.IsLoaded)
            OpenButton.Focus(FocusState.Programmatic);
    }

    internal void Dismiss() => Dismiss(null);

    private void Dismiss(Action? completion)
    {
        if (!shown)
            return;

        shown = false;
        Root.IsHitTestVisible = false;
        afterDismiss = completion;
        StopSlide();
        var from = AppWindow.Position;
        var to = new PointInt32(restingBounds.X + slideOffset.X, restingBounds.Y + slideOffset.Y);
        if (uiSettings.AnimationsEnabled && !from.Equals(to))
        {
            StartSlide(from, to);
        }
        else
        {
            HideMenu();
        }
    }

    private void HideMenu()
    {
        AppWindow.Hide();
        var completion = afterDismiss;
        afterDismiss = null;
        completion?.Invoke();
    }

    private void StartSlide(PointInt32 from, PointInt32 to)
    {
        slide = (from, to);
        slideStart = 0;
        CompositionTarget.Rendering += AdvanceSlide;
    }

    private void AdvanceSlide(object? sender, object args)
    {
        if (slide is not { } current)
            return;

        if (slideStart == 0)
            slideStart = Stopwatch.GetTimestamp();
        double duration = shown ? EntranceMilliseconds : ExitMilliseconds;
        double progress = Stopwatch.GetElapsedTime(slideStart).TotalMilliseconds / duration;
        if (progress >= 1)
        {
            StopSlide();
            if (shown)
                AppWindow.Move(new PointInt32(restingBounds.X, restingBounds.Y));
            else
                HideMenu();
            return;
        }

        double remaining = 1 - progress;
        double eased = shown ? 1 - remaining * remaining * remaining : progress * progress;
        var next = new PointInt32(
            current.From.X + (int)Math.Round((current.To.X - current.From.X) * eased),
            current.From.Y + (int)Math.Round((current.To.Y - current.From.Y) * eased));
        if (!AppWindow.Position.Equals(next))
            AppWindow.Move(next);
    }

    private void StopSlide()
    {
        if (slide is null)
            return;

        CompositionTarget.Rendering -= AdvanceSlide;
        slide = null;
    }

    internal void CloseForExit()
    {
        shown = false;
        StopSlide();
        afterDismiss = null;
        AppWindow.Closing -= OnClosing;
        Close();
    }

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        args.Cancel = true;
        DispatcherQueue.TryEnqueue(Dismiss);
    }

    private void DismissWithKeyboard(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Dismiss();
        Native.Run(new { action = "focus_tray" });
    }

    private void Open(object sender, RoutedEventArgs args) =>
        Dismiss(() => openPanel(false));

    private void OpenSettings(object sender, RoutedEventArgs args) =>
        Dismiss(() => openPanel(true));

    private void Exit(object sender, RoutedEventArgs args) => Dismiss(requestExit);
}
