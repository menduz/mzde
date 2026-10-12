# The default colors and sizes of mzwm. Set mzwm.colors to change them. The
# win-classic theme gives the colors of the title bars and of the 3D borders.
{
  # The desktop below the windows. kwm uses desktop_color of the GTK theme
  # when the theme has it. The lock screen shows this color after a key press.
  desktop = "#29272b";

  bar = {
    background = "#000000";
    foreground = "#a1a1a1";
    # The selected tag and the title of the focused output.
    selected = "#ffffff";
    # The tags without windows.
    emptyTag = "#1d2023";
  };

  border = {
    focus = "#f29718";
    unfocus = "#34373a";
    swallowing = "#ff3333";
  };

  # The widgets of the bar.
  warning = "#ffb354";
  critical = "#ff3333";

  # The lock screen after a wrong password.
  lockFail = "#ff3333";

  # In pixels.
  iconSize = 16;
  spacing = 12;
  gap = 4;
}
