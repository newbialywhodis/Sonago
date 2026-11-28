using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.Windows.Storage.Pickers;
using VirtualKey = Windows.System.VirtualKey;

namespace Sonago;

public sealed partial class PanelWindow : Window
{
    private const int PanelWidth = 368;
    private const int SettingsWidth = 512;
    private const int SettingsHeight = 598;
    private const int SetupHeight = 442;

    private readonly Action shutdown;
    private readonly nint hwnd;
    private readonly DispatcherQueueTimer refreshTimer;
    private readonly DispatcherQueueTimer dismissTimer;
    private bool setup;
    private bool shown;
    private bool active;
    private bool watchingOutsideClicks;
    private bool pickingFolder;
    private bool exiting;
    private string? commandError;
    private string? settingsError;
    private Preferences? draft;
    private MenuFlyout? openMenu;
    private CompactBackdrop? compactBackdrop;

    internal PanelWindow(Action shutdown)
    {
        this.shutdown = shutdown;
        InitializeComponent();
        hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "sonago.ico"));
        var presenter = OverlappedPresenter.CreateForContextMenu();
        AppWindow.SetPresenter(presenter);
        Native.DisableWindowTransitions(hwnd);
        AppWindow.Closing += OnClosing;
        Activated += OnActivated;

        var escape = new KeyboardAccelerator { Key = VirtualKey.Escape };
        escape.Invoked += OnEscape;
        Root.KeyboardAccelerators.Add(escape);
        Root.Loaded += (_, _) => PresentSettingsPopup();
        foreach (var (button, menu) in new (SplitButton, MenuFlyout)[]
                 {
                     (SaveButton, SaveMenu), (Microphone, MicrophoneMenu), (SystemAudio, SystemMenu)
                 })
        {
            menu.SystemBackdrop = new DesktopAcrylicBackdrop();
            menu.Opened += (_, _) => SetChevronRotation(button, 180);
            menu.Closed += (_, _) => SetChevronRotation(button, 0);
        }

        refreshTimer = DispatcherQueue.CreateTimer();
        refreshTimer.Interval = TimeSpan.FromSeconds(1);
        refreshTimer.Tick += (_, _) => Refresh();
        dismissTimer = DispatcherQueue.CreateTimer();
        dismissTimer.Interval = TimeSpan.FromMilliseconds(180);
        dismissTimer.IsRepeating = false;
        dismissTimer.Tick += (_, _) => DismissIfInactive();
    }

    internal void ShowSetup()
    {
        setup = true;
        OpenSettings();
        Show(newAnchor: true);
    }

    internal void ToggleFromTray(bool activate)
    {
        if (shown && !setup && !pickingFolder)
        {
            Hide();
        }
        else if (!pickingFolder)
        {
            Show(newAnchor: !shown, activate: activate || setup);
        }
    }

    internal void ShowFromTray(bool settings)
    {
        if (pickingFolder)
        {
            return;
        }
        if (settings && draft is null)
        {
            OpenSettings();
        }
        Show(newAnchor: !shown);
    }

    private void Show(bool newAnchor, bool activate = true)
    {
        dismissTimer.Stop();
        AppWindow.IsShownInSwitchers = setup;
        var presenter = (OverlappedPresenter)AppWindow.Presenter;
        if (!setup && presenter.HasBorder)
        {
            presenter.SetBorderAndTitleBar(false, false);
            Native.ConfigurePanelFrame(hwnd);
            compactBackdrop = new CompactBackdrop(hwnd, CompactSurface);
            SystemBackdrop = compactBackdrop;
        }
        presenter.IsAlwaysOnTop = !setup;
        QuickPanel.Visibility = setup ? Visibility.Collapsed : Visibility.Visible;
        Root.IsHitTestVisible = true;
        Title = setup ? "Set up Sonago" : "Sonago";
        shown = true;
        Refresh(resize: false);
        Resize(newAnchor, slideIn: newAnchor && !setup && draft is null);
        if (!activate && Native.Call<bool>(new { action = "watch_outside_clicks", watch = true }))
        {
            watchingOutsideClicks = true;
            AppWindow.Show(activateWindow: false);
        }
        else
        {
            StopWatchingOutsideClicks();
            Activate();
        }
        refreshTimer.Start();
        DispatcherQueue.TryEnqueue(() =>
        {
            if (shown && !watchingOutsideClicks)
            {
                if (setup)
                    SystemEnabled.Focus(FocusState.Programmatic);
                else if (draft is not null)
                    PresentSettingsPopup();
                else
                    Gear.Focus(FocusState.Programmatic);
            }
        });
    }

    private void Hide()
    {
        if (!shown)
        {
            return;
        }
        shown = false;
        Root.IsHitTestVisible = false;
        StopWatchingOutsideClicks();
        dismissTimer.Stop();
        refreshTimer.Stop();
        openMenu?.Hide();
        CloseSettings(restoreFocus: false);
        commandError = null;
        SlideOut();
    }

    internal void DismissPassivePanel()
    {
        if (watchingOutsideClicks)
            Hide();
    }

    private void StopWatchingOutsideClicks()
    {
        if (watchingOutsideClicks)
        {
            watchingOutsideClicks = false;
            Native.Run(new { action = "watch_outside_clicks", watch = false });
        }
    }

    internal void CloseForExit()
    {
        exiting = true;
        shown = false;
        StopWatchingOutsideClicks();
        refreshTimer.Stop();
        dismissTimer.Stop();
        StopSlide();
        Close();
    }

    internal void Refresh() => Refresh(resize: true);

    internal void OnDesktopLayoutChanged()
    {
        if (shown)
            Resize();
    }

    private void Refresh(bool resize)
    {
        if (!shown)
        {
            return;
        }
        var state = Native.Call<Snapshot>(new { action = "snapshot" });
        Microphone.IsChecked = state.MicOn;
        MicrophoneIcon.Glyph = state.MicOn ? "\uE720" : "\uEC54";
        SystemAudio.IsChecked = state.SystemOn;
        SystemAudioIcon.Glyph = state.SystemOn ? "\uE767" : "\uE74F";
        SaveButton.IsEnabled = state.CanSave;
        ToolTipService.SetToolTip(SaveButton, state.SaveLabel);
        AutomationProperties.SetHelpText(SaveButton, state.SaveLabel);
        SyncSourceEnabled(0, SystemEnabled, state.SystemOn);
        SyncSourceEnabled(1, MicEnabled, state.MicOn);
        string warning = commandError ?? state.Warning ?? "";
        bool needsResize = PanelWarning.Text != warning;
        PanelWarning.Text = warning;
        PanelWarning.Visibility = warning.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        UpdateSettingsNotice(settingsError ?? draft?.Issue ?? state.Issue);
        if (resize && needsResize && !setup)
        {
            Resize();
        }
    }

    private void SyncSourceEnabled(int track, CheckBox field, bool enabled)
    {
        if (draft is null || draft.Settings.Sources[track].Enabled == enabled)
            return;

        // Refresh also sees a source switched off when writing the settings file fails.
        draft.Settings.Sources[track] = draft.Settings.Sources[track] with { Enabled = enabled };
        field.IsChecked = enabled;
    }

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (exiting)
        {
            return;
        }
        args.Cancel = true;
        if (!pickingFolder)
        {
            DispatcherQueue.TryEnqueue(Dismiss);
        }
    }

    private void OnActivated(object sender, WindowActivatedEventArgs e)
    {
        active = e.WindowActivationState != WindowActivationState.Deactivated;
        if (active)
        {
            StopWatchingOutsideClicks();
            dismissTimer.Stop();
        }
        else if (shown && !setup && !pickingFolder)
        {
            // Explorer delivers the tray toggle on button-up, after deactivation.
            if (Native.Call<bool>(new { action = "pointer_over_tray", require_press = false }))
                dismissTimer.Start();
            else
                DispatcherQueue.TryEnqueue(DismissIfInactive);
        }
    }

    private void DismissIfInactive()
    {
        if (shown && !active && !watchingOutsideClicks && !setup && !pickingFolder && openMenu is null)
        {
            // A WinUI input or popup window can hold focus while the host is deactivated.
            if (Native.OwnsForegroundWindow(hwnd) ||
                Native.Call<bool>(new { action = "pointer_over_tray", require_press = true }))
                dismissTimer.Start();
            else
                Hide();
        }
    }

    private void Dismiss()
    {
        if (setup)
            Quit(this, new RoutedEventArgs());
        else
            Hide();
    }

    private void OnEscape(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (!shown || pickingFolder)
        {
            return;
        }
        args.Handled = true;
        if (openMenu is not null)
            openMenu.Hide();
        else if (!setup && draft is not null)
            CloseSettings();
        else
            Dismiss();
    }

    private void ToggleMicrophone(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "toggle_source", track = 1 });

    private void ToggleSystem(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "toggle_source", track = 0 });

    private void Save(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "save" });

    private void OpenMicrophoneMenu(object sender, object args) => OpenDeviceMenu(MicrophoneMenu, 1);
    private void OpenSystemMenu(object sender, object args) => OpenDeviceMenu(SystemMenu, 0);

    private void OpenDeviceMenu(MenuFlyout menu, int track)
    {
        openMenu = menu;
        menu.Items.Clear();
        try
        {
            var devices = Native.Call<DeviceChoice[]>(new { action = "devices", track });
            foreach (var device in devices)
            {
                var item = new RadioMenuFlyoutItem
                {
                    Text = device.Label,
                    GroupName = track == 0 ? "SystemOutput" : "MicrophoneInput",
                    IsChecked = device.Selected
                };
                item.Click += (_, _) => SelectDevice(track, devices, device.Endpoint);
                menu.Items.Add(item);
            }
        }
        catch (InvalidOperationException error)
        {
            menu.Items.Add(new MenuFlyoutItem { Text = "Could not list audio devices", IsEnabled = false });
            commandError = error.Message;
            Refresh();
        }
    }

    private void SelectDevice(int track, DeviceChoice[] devices, string? endpoint)
    {
        if (QuickCommand(new { action = "select_device", track, endpoint }) && draft is not null)
        {
            draft.Settings.Sources[track] = draft.Settings.Sources[track] with { Endpoint = endpoint };
            SetDeviceChoices(track, devices, endpoint);
        }
    }

    private void OpenSaveMenu(object sender, object args)
    {
        openMenu = SaveMenu;
        SaveMenu.Items.Clear();
        foreach (var length in Native.Call<SaveLength[]>(new { action = "save_lengths" }))
        {
            var item = new MenuFlyoutItem { Text = length.Label };
            if (length.IsDefault)
            {
                item.Icon = new SymbolIcon(Symbol.Accept);
            }
            item.Click += (_, _) => QuickCommand(new { action = "save", length.Minutes });
            SaveMenu.Items.Add(item);
        }
    }

    private void CloseMenu(object sender, object args)
    {
        if (ReferenceEquals(openMenu, sender))
        {
            openMenu = null;
        }
        DispatcherQueue.TryEnqueue(DismissIfInactive);
    }

    private void SetChevronRotation(SplitButton button, float degrees)
    {
        // Reuse the chevron from the WinUI split button template.
        if (VisualTreeHelper.GetChildrenCount(button) == 0 ||
            VisualTreeHelper.GetChild(button, 0) is not Grid root ||
            root.Children.OfType<Button>().FirstOrDefault(part => part.Name == "SecondaryButton")
                is not { Content: FrameworkElement chevron })
            return;

        var visual = ElementCompositionPreview.GetElementVisual(chevron);
        const string rotation = nameof(visual.RotationAngleInDegrees);
        visual.CenterPoint = new((float)(chevron.ActualWidth / 2), (float)(chevron.ActualHeight / 2), 0);
        if (uiSettings.AnimationsEnabled)
        {
            var animation = visual.Compositor.CreateScalarKeyFrameAnimation();
            animation.Duration = TimeSpan.FromMilliseconds(180);
            animation.InsertKeyFrame(1, degrees);
            visual.StartAnimation(rotation, animation);
        }
        else
        {
            visual.StopAnimation(rotation);
            visual.RotationAngleInDegrees = degrees;
        }
    }

    private bool QuickCommand(object command)
    {
        if (!shown)
        {
            return false;
        }
        commandError = null;
        try
        {
            Native.Run(command);
        }
        catch (InvalidOperationException error)
        {
            commandError = error.Message;
        }
        Refresh();
        return commandError is null;
    }

    private void ToggleSettings(object sender, RoutedEventArgs args)
    {
        if (!shown)
        {
            return;
        }
        if (draft is not null)
        {
            CloseSettings();
        }
        else
        {
            OpenSettings();
            Refresh();
            PresentSettingsPopup();
        }
    }

    private void PresentSettingsPopup()
    {
        if (!shown || setup || draft is null || !Root.IsLoaded || SettingsPopup.IsOpen)
            return;

        if (slideFrom.HasValue)
            FinishSlide();
        if (SettingsPopup.Child is null)
        {
            Root.Children.Remove(SettingsPanel);
            SettingsPanel.Visibility = Visibility.Visible;
            SettingsPopup.Child = SettingsPanel;
        }

        LayoutSettingsPopup();
        SettingsPopup.IsOpen = true;
    }

    private void LayoutSettingsPopup()
    {
        var place = placement!;
        var work = place.WorkArea;
        bool below = place.Edge == TaskbarEdge.Top;
        SettingsPopup.DesiredPlacement = place.Edge switch
        {
            TaskbarEdge.Left => PopupPlacementMode.TopEdgeAlignedLeft,
            TaskbarEdge.Top => PopupPlacementMode.BottomEdgeAlignedRight,
            _ => PopupPlacementMode.TopEdgeAlignedRight
        };
        SettingsPopup.VerticalOffset = below ? 4 : -4;
        int availableHeight = below ? work.Bottom - place.Y - place.Height : place.Y - work.Top;
        SettingsPanel.Width = Math.Clamp((work.Right - work.Left) / panelScale - 24, 1, SettingsWidth);
        SettingsPanel.Height = Math.Clamp(availableHeight / panelScale - 16, 1, SettingsHeight);
    }

    private void OnSettingsPopupOpened(object sender, object args)
    {
        if (SettingsPopup.IsOpen)
            SystemEnabled.Focus(FocusState.Programmatic);
    }

    private void OpenSettings()
    {
        draft = Native.Call<Preferences>(new { action = "settings" });
        var settings = draft.Settings;
        SettingsTitle.Text = setup ? "Set up Sonago" : "Settings";
        SetupDescription.Visibility = setup ? Visibility.Visible : Visibility.Collapsed;
        FormatRow.Visibility = FolderRow.Visibility = General.Visibility = QuitButton.Visibility =
            setup ? Visibility.Collapsed : Visibility.Visible;
        ApplyButton.Content = setup ? "Start capturing" : "Apply";
        CancelButton.Content = setup ? "Exit" : "Cancel";
        SystemEnabled.IsChecked = settings.Sources[0].Enabled;
        MicEnabled.IsChecked = settings.Sources[1].Enabled;
        SetDeviceChoices(0, draft.Devices[0], settings.Sources[0].Endpoint);
        SetDeviceChoices(1, draft.Devices[1], settings.Sources[1].Endpoint);
        Retention.Value = settings.Retention;
        Minutes.Value = settings.Minutes;
        Hotkey.Text = settings.Hotkey;
        DoubleTap.SelectedItem = DoubleTap.Items.OfType<ComboBoxItem>()
            .FirstOrDefault(item => Equals(item.Tag, settings.DoubleTap));
        Folder.Text = settings.Output;
        Format.SelectedIndex = settings.Format == "ogg_opus" ? 1 : 0;
        Autostart.IsChecked = settings.Autostart;
        Notifications.IsChecked = settings.Notifications;
        SaveSoundChoice.ItemsSource = draft.SaveSoundChoices;
        SaveSoundChoice.SelectedIndex = settings.SaveSound;
        Transcribe.IsChecked = settings.Transcribe;
        if (setup)
            SettingsPanel.Visibility = Visibility.Visible;
        Gear.IsChecked = true;
        ToolTipService.SetToolTip(Gear, "Close settings");
        UpdateSettingsNotice(draft.Issue);
    }

    private void CloseSettings(bool restoreFocus = true)
    {
        if (draft is null || (restoreFocus && !shown))
        {
            return;
        }
        SettingsPopup.IsOpen = false;
        draft = null;
        settingsError = null;
        if (SettingsPopup.Child is null)
            SettingsPanel.Visibility = Visibility.Collapsed;
        Gear.IsChecked = false;
        ToolTipService.SetToolTip(Gear, "Settings");
        if (restoreFocus)
        {
            Refresh();
            Gear.Focus(FocusState.Programmatic);
        }
    }

    private async void Browse(object sender, RoutedEventArgs args)
    {
        pickingFolder = true;
        try
        {
            var result = await new FolderPicker(AppWindow.Id).PickSingleFolderAsync();
            if (result is not null)
            {
                Folder.Text = result.Path;
                settingsError = null;
            }
        }
        catch (Exception error)
        {
            settingsError = error.Message;
        }
        finally
        {
            pickingFolder = false;
        }
        Refresh();
    }

    private void OnSaveSoundChanged(object sender, SelectionChangedEventArgs args) =>
        PlaySaveSound.IsEnabled = SaveSoundChoice.SelectedIndex > 0;

    private void PreviewSaveSound(object sender, RoutedEventArgs args)
    {
        try
        {
            Native.Run(new { action = "preview_save_sound", sound = SaveSoundChoice.SelectedIndex });
        }
        catch (InvalidOperationException error)
        {
            settingsError = error.Message;
            Refresh();
        }
    }

    private void Apply(object sender, RoutedEventArgs args)
    {
        if (!shown || draft is null)
        {
            return;
        }
        var original = draft.Settings;
        try
        {
            var settings = original with
            {
                Sources =
                [
                    new Source(SystemEnabled.IsChecked == true, SelectedEndpoint(0, SystemDevice)),
                    new Source(MicEnabled.IsChecked == true, SelectedEndpoint(1, MicDevice))
                ],
                Retention = ReadMinutes(Retention, "history limit"),
                Minutes = ReadMinutes(Minutes, "default save length"),
                Output = Folder.Text == original.Output ? original.Output : Folder.Text.Trim(),
                Format = Format.SelectedIndex == 1 ? "ogg_opus" : "matroska",
                Hotkey = Hotkey.Text.Trim(),
                DoubleTap = (DoubleTap.SelectedItem as ComboBoxItem)?.Tag as string ?? original.DoubleTap,
                Autostart = Autostart.IsChecked == true,
                Notifications = Notifications.IsChecked == true,
                SaveSound = SaveSoundChoice.SelectedIndex >= 0 ? SaveSoundChoice.SelectedIndex : original.SaveSound,
                Transcribe = Transcribe.IsChecked == true
            };
            Native.Run(new { action = "apply", settings });
        }
        catch (InvalidOperationException error)
        {
            settingsError = error.Message;
            Refresh();
            return;
        }
        if (setup)
        {
            setup = false;
            Hide();
        }
        else
        {
            commandError = null;
            CloseSettings();
        }
    }

    private void SetDeviceChoices(int track, DeviceChoice[] devices, string? endpoint)
    {
        var combo = track == 0 ? SystemDevice : MicDevice;
        draft!.Devices[track] = devices;
        combo.ItemsSource = devices.Select(device => device.Label).ToArray();
        combo.SelectedIndex = Array.FindIndex(devices, device => device.Endpoint == endpoint);
    }

    private string? SelectedEndpoint(int track, ComboBox combo) =>
        combo.SelectedIndex >= 0 ? draft!.Devices[track][combo.SelectedIndex].Endpoint
            : draft!.Settings.Sources[track].Endpoint;

    private static int ReadMinutes(NumberBox box, string label)
    {
        double value = box.Value;
        if (!(value is >= 1 and <= 720) || Math.Truncate(value) != value)
        {
            throw new InvalidOperationException($"Enter a whole number from 1 to 720 for the {label}.");
        }
        return (int)value;
    }

    private void Cancel(object sender, RoutedEventArgs args) => DismissSettings();

    private void DismissSettings()
    {
        if (setup)
            Dismiss();
        else
            CloseSettings();
    }

    private void Quit(object sender, RoutedEventArgs args)
    {
        try
        {
            Native.Run(new { action = "can_quit" });
        }
        catch (InvalidOperationException error)
        {
            settingsError = error.Message;
            Refresh();
            return;
        }
        shutdown();
    }

    private void UpdateSettingsNotice(string? issue)
    {
        SettingsNotice.Message = issue ?? "";
        SettingsNotice.Severity = settingsError is null ? InfoBarSeverity.Warning : InfoBarSeverity.Error;
        SettingsNotice.IsOpen = draft is not null && issue is not null;
    }
}
