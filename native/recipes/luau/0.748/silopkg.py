import shutil

from silo import Component, Package, SiloPackage
from silo.enums import Platform
from silo.tools.cmake import CMakeBuild, CMakeGenerator

# Luau's type checker and what it links, in link order. The same Luau release mlua runs (luau0-src 0.24.1+luau748).
ANALYSIS_LIBRARIES = ("Luau.Analysis", "Luau.Config", "Luau.Compiler", "Luau.Inliner", "Luau.Bytecode", "Luau.VM", "Luau.Ast", "Luau.Common")
# Folders under the source holding public headers, all under include/Luau
HEADER_FOLDERS = ("Common", "Ast", "Config", "Analysis", "Compiler", "Bytecode", "VM")


class Luau(SiloPackage):
    """Luau built from source, for its Analysis library. Upstream's CMake has no install step, so build() copies the results."""

    name = "luau"
    version = "0.748"

    def source(self, src):
        src.git("https://github.com/luau-lang/luau.git", commit="3d91c9d5c280c95bfc4be63aac46012cf861d394")

    def deps(self):
        self.tool("cmake@4.3")

    def generate(self):
        toolchain = CMakeGenerator(self)
        toolchain.add_cache_bool("LUAU_BUILD_CLI", False)
        toolchain.add_cache_bool("LUAU_BUILD_TESTS", False)
        toolchain.generate()

    def build(self):
        cmake = CMakeBuild(self)
        cmake.configure()
        cmake.build()
        include = self.install_dir / "include"
        for folder in HEADER_FOLDERS:
            shutil.copytree(self.source_dir / folder / "include", include, dirs_exist_ok=True)
        lib = self.install_dir / "lib"
        lib.mkdir(parents=True, exist_ok=True)
        for name in ANALYSIS_LIBRARIES:
            built = self.build_dir / self._library_file(name)
            if not built.is_file():
                raise FileNotFoundError(f"luau's build made no {built.name}")
            shutil.copy2(built, lib / built.name)
        shutil.copy2(self.source_dir / "LICENSE.txt", self.install_dir / "LICENSE.txt")

    def _library_file(self, name: str) -> str:
        return f"{name}.lib" if self.profile.settings.os is Platform.Windows else f"lib{name}.a"

    def package(self):
        analysis = Component("analysis", headers=["include"], static_libs=list(ANALYSIS_LIBRARIES))
        return Package(components=[analysis], licenses=["LICENSE.txt"])
