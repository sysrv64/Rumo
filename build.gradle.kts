plugins {
    alias(libs.plugins.android.application) apply false
    // The assistant's module is an Android library. Declared here, next to the
    // application plugin, so AGP is loaded once from the root with a known
    // version: requesting it from the module alone fails, because the same
    // artifact is already on the classpath through `android.application`.
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.compose) apply false
}
