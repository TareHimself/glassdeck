import shutil

from silo import Component, Package, SharedLib, SiloPackage
from silo.enums import Platform, RuntimeLinkage
from silo.tools.cmake import CMakeBuild, CMakeGenerator

# libcef.dll loads these from beside the program
RUNTIME_DLLS = ("chrome_elf.dll", "d3dcompiler_47.dll", "dxcompiler.dll", "vk_swiftshader.dll", "vulkan-1.dll")
RELEASE_DATA = ("v8_context_snapshot.bin", "vk_swiftshader_icd.json")
RESOURCE_DATA = ("icudtl.dat", "resources.pak", "chrome_100_percent.pak", "chrome_200_percent.pak")


class Cef(SiloPackage):
    """Chromium Embedded Framework: Spotify's prebuilt libcef plus its C++ wrapper, built here."""

    name = "cef"
    version = "156.0.32"
    platforms = (Platform.Windows,)

    def source(self, src):
        src.fetch(
            "https://cef-builds.spotifycdn.com/"
            "cef_binary_156.0.32%2Bg3d1f4ab%2Bchromium-156.0.8078.109_windows64_minimal.tar.bz2",
            sha256="6c03b3494d02e9c1a72994c8a5c3e0fddb25a59898166a054b896a870da1ff4c",
        )

    def deps(self):
        self.tool("cmake@4.3")

    def generate(self):
        toolchain = CMakeGenerator(self)
        # CEF picks the C runtime itself and defaults to the static one
        static = self.profile.settings.runtime is RuntimeLinkage.Static
        toolchain.add_cache_string("CEF_RUNTIME_LIBRARY_FLAG", "/MT" if static else "/MD")
        # Debug info inside the library, so linkers don't look for a .pdb the package lacks (LNK4099)
        toolchain.add_cache_string("CEF_DEBUG_INFO_FLAG", "/Z7")
        # The minimal build ships no cef_sandbox.lib
        toolchain.add_cache_bool("USE_SANDBOX", False)
        toolchain.generate()

    def build(self):
        CMakeBuild(self).configure()
        self.run(["cmake", "--build", self.build_dir, "--target", "libcef_dll_wrapper"])

        # CEF has no install rules, so the package is laid out here
        out = self.install_dir
        # CEF's headers include each other as "include/cef_app.h"
        shutil.copytree(self.source_dir / "include", out / "include" / "include")
        (out / "lib").mkdir(parents=True)
        shutil.copy2(self.build_dir / "libcef_dll_wrapper" / "libcef_dll_wrapper.lib", out / "lib")
        shutil.copy2(self.source_dir / "Release" / "libcef.lib", out / "lib")
        (out / "bin").mkdir()
        for dll in ("libcef.dll", *RUNTIME_DLLS):
            shutil.copy2(self.source_dir / "Release" / dll, out / "bin")
        data = out / "share" / "cef"
        data.mkdir(parents=True)
        for name in RELEASE_DATA:
            shutil.copy2(self.source_dir / "Release" / name, data)
        for name in RESOURCE_DATA:
            shutil.copy2(self.source_dir / "Resources" / name, data)
        shutil.copytree(self.source_dir / "Resources" / "locales", data / "locales")

    def package(self):
        cef = Component(
            "cef",
            headers=["include"],
            static_libs=["lib/libcef_dll_wrapper.lib"],
            shared_libs=[
                SharedLib("bin/libcef.dll", import_lib="lib/libcef.lib"),
                *(SharedLib(f"bin/{dll}") for dll in RUNTIME_DLLS),
            ],
            # Data files CEF reads from beside the program, like its DLLs
            content=["share/cef"],
        )
        return Package(components=[cef], licenses=["LICENSE.txt", "CREDITS.html"])
