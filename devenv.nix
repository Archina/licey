{ pkgs, lib, config, inputs, ... }:

let
  # Shared libraries the app loads at runtime. winit resolves several of these
  # with dlopen (e.g. libwayland-client), so they must be on LD_LIBRARY_PATH,
  # not merely present in `packages`.
  runtimeLibraries = with pkgs; [
    vulkan-loader
    libglvnd # libEGL / libGL
    libxkbcommon
    wayland # libwayland-client / -cursor / -egl
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
{
  # https://devenv.sh/basics/
  env.GREET = "devenv";

  # https://devenv.sh/packages/
  packages = with pkgs; [
    # DevEnv Default
    git

    # Old NativeBuildInputs
    pkg-config

    mold
    clang
    
    # Old BuildInputs
    udev
    alsa-lib-with-plugins
    gcc

    # Synthetic input for verifying the UI headlessly. On this KWin Wayland
    # session only `dotool` works end to end: it writes to /dev/uinput (the
    # uaccess ACL grants the logged-in user write access) and supports
    # press/release, so it can drag. `wtype` needs zwp_virtual_keyboard_v1,
    # which KWin does not implement; `ydotool` needs a running ydotoold and
    # cannot hold a button across moves; `kdotool` covers window focus and
    # geometry through KWin scripting. All four are kept so the setup also
    # works on wlroots compositors. See scripts/ui-input.sh.
    wtype
    ydotool
    dotool
    kdotool
  ]
  ++ runtimeLibraries;

  # https://devenv.sh/languages/
  languages.rust = {
    enable = true;
    channel = "nightly";
    components = [ "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer" ];
  };

  # https://devenv.sh/processes/
  # processes.dev.exec = "${lib.getExe pkgs.watchexec} -n -- ls -la";

  # ydotool only talks to a daemon; `devenv up` starts it on the default socket
  # ($XDG_RUNTIME_DIR/.ydotool_socket), which is where the client looks.
  processes.ydotoold.exec = "ydotoold";

  # https://devenv.sh/services/
  # services.postgres.enable = true;

  # https://devenv.sh/scripts/
  scripts.hello.exec = ''
    echo hello from $GREET
  '';

  # https://devenv.sh/basics/
  enterShell = ''
    hello         # Run scripts directly
    git --version # Use packages
    export LD_LIBRARY_PATH=${lib.makeLibraryPath runtimeLibraries}:$LD_LIBRARY_PATH
  '';

  # https://devenv.sh/tasks/
  tasks = {
  #   "myproj:setup".exec = "mytool build";
  #   "devenv:enterShell".after = [ "myproj:setup" ];
    "dlc:run".exec = "cargo run";
  };

  # https://devenv.sh/tests/
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
  '';

  # https://devenv.sh/git-hooks/
  # git-hooks.hooks.shellcheck.enable = true;

  # See full reference at https://devenv.sh/reference/options/
}
