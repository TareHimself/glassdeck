import zipfile

from silo import Component, InvalidConfiguration, Package, SiloPackage
from silo.enums import Architecture, Platform
from silo.source import download

RELEASES = "https://github.com/JohnnyMorganz/luau-lsp/releases/download"
# The release archive for each system, with its sha256 as GitHub lists it
ARCHIVES = {
    (Platform.Windows, Architecture.X86_64): ("win64", "64225b9738102a30de6ee09fb28e5a3827edc402ecf2a6364af570312468159f"),
    (Platform.Linux, Architecture.X86_64): ("linux-x86_64", "1a2ea1ae4f98f8946cefd970a4b54853e11ab4775e6932faa7f60cf920346567"),
    (Platform.Linux, Architecture.ARM64): ("linux-arm64", "6062345b95124ca937b8c324b74bc3fddb1d6919ba133c58f8b17945ce1acbc2"),
    (Platform.Darwin, Architecture.X86_64): ("macos", "7d3936e8dec6dc77547abd061d2e950da392562a5f94858b880f23dba50d84cd"),
    (Platform.Darwin, Architecture.ARM64): ("macos", "7d3936e8dec6dc77547abd061d2e950da392562a5f94858b880f23dba50d84cd"),
}


class LuauLsp(SiloPackage):
    """The Luau language server, as upstream's prebuilt release. Nothing is compiled."""

    name = "luau-lsp"
    version = "1.70.1"
    platforms = (Platform.Windows, Platform.Linux, Platform.Darwin)

    @property
    def system(self) -> tuple[Platform, Architecture]:
        return self.profile.settings.os, self.profile.settings.arch

    @property
    def executable(self) -> str:
        return "luau-lsp.exe" if self.profile.settings.os == Platform.Windows else "luau-lsp"

    def source(self, src):
        # src.fetch cannot unpack a zip archive, so build() downloads it
        src.empty()

    def validate(self):
        if self.system not in ARCHIVES:
            raise InvalidConfiguration(f"luau-lsp {self.version} has no release archive listed here")

    def build(self):
        name, sha256 = ARCHIVES[self.system]
        archive = download(f"{RELEASES}/{self.version}/luau-lsp-{name}.zip", sha256)
        with zipfile.ZipFile(archive) as release:
            release.extract(self.executable, self.install_dir / "bin")

    def package(self):
        return Package(components=[Component("luau-lsp", executables=[f"bin/{self.executable}"])])
