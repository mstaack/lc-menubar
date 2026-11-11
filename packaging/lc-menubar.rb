# Homebrew cask for github.com/mstaack/homebrew-tap (Casks/lc-menubar.rb).
# Update `version` and `sha256` from each release's .sha256 file.
cask "lc-menubar" do
  version "0.1.0"
  sha256 "REPLACE_WITH_SHA256"

  url "https://github.com/mstaack/lc-menubar/releases/download/v#{version}/lc-menubar-#{version}-macos-universal.zip"
  name "LC Menubar"
  desc "Menu bar companion for Laravel Cloud deployments, metrics and spend"
  homepage "https://github.com/mstaack/lc-menubar"

  # LC Menubar updates itself; brew should not fight it.
  auto_updates true
  depends_on macos: ">= :ventura"

  app "LC Menubar.app"

  # Releases are not notarized yet: without this, macOS refuses to open the app.
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-dr", "com.apple.quarantine", "#{appdir}/LC Menubar.app"]
  end

  uninstall launchctl: "io.github.mstaack.lc-menubar",
            quit:      "io.github.mstaack.lc-menubar",
            delete:    "~/Library/LaunchAgents/io.github.mstaack.lc-menubar.plist"

  zap trash: "~/Library/Application Support/lc-menubar"
end
