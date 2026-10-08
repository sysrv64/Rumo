pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "Rumo"
include(":app")
// The AI layer lives in its own repository and is pulled in as `ai-engines/`.
// That repository is a container for several engines; the Rumo assistant lives
// one level down, under `rumo/`. The project name stays `:ai-engines`, so
// nothing downstream depends on where the sources sit, and the two lines move
// together if the layout changes. A plain module inclusion, so nothing about the
// main build depends on how that checkout got there — see AGENTS.md.
include(":ai-engines")
project(":ai-engines").projectDir = file("ai-engines/rumo")
