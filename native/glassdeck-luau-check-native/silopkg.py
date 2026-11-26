from silo import Component, Package, SiloPackage
from silo.tools.cmake import CMakeBuild, CMakeDeps, CMakeGenerator, CMakePresets


class GlassdeckLuauCheckNative(SiloPackage):
    name = "glassdeck-luau-check-native"
    version = "1.0.0"

    def source(self, src):
        src.local(".")

    def deps(self):
        self.dep("luau@0.748")
        self.tool("cmake@4.3")

    def generate(self):
        CMakeDeps(self).generate()
        toolchain = CMakeGenerator(self)
        toolchain.generate()
        configuration = "Debug" if self.is_debug else "Release"
        presets = CMakePresets(self, toolchain)
        presets.add(f"Glassdeck-{configuration}", build_dir=f"build-{configuration.lower()}")
        presets.generate()

    def build(self):
        cmake = CMakeBuild(self)
        cmake.configure()
        cmake.build()
        cmake.install()

    def package(self):
        return Package(components=[Component(self.name, content=["bin"])])
