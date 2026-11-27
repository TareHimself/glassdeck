import shutil

from silo import Component, Package, SiloPackage
from silo.enums import Platform

# Each server's package and its executable, as that package exports it
SERVERS = {"luau-lsp": "luau-lsp", "slang": "slangd"}


class GlassdeckLanguageServers(SiloPackage):
    """The language servers the desktop app's editors run, gathered into one bin folder for build.py."""

    name = "glassdeck-language-servers"
    version = "1.0.0"

    def source(self, src):
        src.empty()

    def deps(self):
        self.dep("luau-lsp@1.70.1")
        self.dep("slang@2025.9.2")

    def build(self):
        suffix = ".exe" if self.profile.settings.os == Platform.Windows else ""
        bin_dir = self.install_dir / "bin"
        bin_dir.mkdir(parents=True, exist_ok=True)
        for package, executable in SERVERS.items():
            store = self.context.dependency_packages[package]
            shutil.copy2(store / "bin" / f"{executable}{suffix}", bin_dir)

    def package(self):
        return Package(components=[Component(self.name, content=["bin"])])
