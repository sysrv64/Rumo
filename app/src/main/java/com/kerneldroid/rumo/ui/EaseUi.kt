// SPDX-License-Identifier: Apache-2.0
//
// The one Kotlin file in this module that is not GPL, and deliberately so.
//
// It is a port of the easing in rumo-rs/rumo-core/src/ease.rs, which is
// Apache-2.0. A derivative of Apache-2.0 code may be distributed under
// GPL-3.0-or-later, so this could be relicensed to match its neighbours — but
// that is a one-way door: GPL text cannot be taken back to Apache, and the
// easing curves are the sort of thing worth being able to lift into a
// permissively licensed project later. Nothing is lost by leaving it: the APK
// is a combined work and ships under GPL-3.0-or-later regardless, and the
// attribution for the original stays in NOTICE.
//
// Keep this header, and keep the two implementations in step; the comment
// below says why that matters.
package com.kerneldroid.rumo.ui

/**
 * The timing function of one segment between keys: the shape of the motion, not the values.
 *
 * Linear interpolation between two keys is exactly what makes handmade animation look
 * mechanical: real motion eases out slowly, travels and settles, and two control points of a
 * cubic Bézier are enough to describe all of that in two numbers at the end.
 *
 * A named set (`ease-in`, `ease-out`, `bounce`) cannot be tuned: as soon as you want the
 * settle to be *slightly* harder, there is nothing to turn. The cubic
 * Bézier shape — two control points with the ends pinned at `(0,0)` and
 * `(1,1)`, the same as CSS `cubic-bezier()` — is one curve that names
 * them all: the presets below are ordinary values of the same type, not separate
 * cases. `y` is **not** clamped to `0..=1`: letting it pass beyond one and come back
 * is exactly the overshoot, and overshoot is what "snappy" means for a layer that lands.
 *
 * [Linear] and [Hold] are values too, not `null`: the sampler has one shape on
 * input, and the document has one field.
 *
 * **The formula must be literally the same as in Rust `rumo_core::ease`**
 * (`cubic_bezier`): the same operations in the same order. A divergence between here and there
 * is a frame that looks different in the preview and in the export, and it is noticeable
 * only by eye. The `EaseUiTest` test checks the value table against the ones that
 * Rust pins.
 */
data class EaseUi(
    val kind: Kind = Kind.LINEAR,
    val x1: Float = 0f,
    val y1: Float = 0f,
    val x2: Float = 0f,
    val y2: Float = 0f,
) {
    enum class Kind { LINEAR, HOLD, CUBIC }

    /** Progress `t` (0..=1) after the curve. */
    fun at(t: Float): Float {
        // A non-finite `t` reads as 0: this value is multiplied by the layer's
        // offset, and a NaN would take the layer off-frame rather than "look odd".
        val p = if (t.isFinite()) t.coerceIn(0f, 1f) else 0f
        return when (kind) {
            Kind.LINEAR -> p
            // A step: the value is held for the whole segment and jumps at the next
            // key. The jump belongs to the *end* of the segment, so exactly at
            // `t = 1` the answer is already the next key's value: otherwise the held
            // key would keep holding at the very moment the
            // next one began, and they would diverge at the frame boundary.
            Kind.HOLD -> if (p >= 1f) 1f else 0f
            Kind.CUBIC -> cubicBezier(p, x1, y1, x2, y2)
        }
    }

    /** A straight line — so the sampler can take exactly the arithmetic it had before. */
    fun isLinear(): Boolean = kind == Kind.LINEAR

    /** The preset name, if the curve is exactly one of them; otherwise `null`. */
    fun presetName(): String? = PRESETS.firstOrNull { it.second == this }?.first

    companion object {
        /** CSS `ease`: slow start, slow settle. */
        val EASE = EaseUi(Kind.CUBIC, 0.25f, 0.1f, 0.25f, 1f)

        /** CSS `ease-in`: starts slowly, arrives at full speed. */
        val EASE_IN = EaseUi(Kind.CUBIC, 0.42f, 0f, 1f, 1f)

        /** CSS `ease-out`: starts fast, settles. */
        val EASE_OUT = EaseUi(Kind.CUBIC, 0f, 0f, 0.58f, 1f)

        /** CSS `ease-in-out`. */
        val EASE_IN_OUT = EaseUi(Kind.CUBIC, 0.42f, 0f, 0.58f, 1f)

        /**
         * Overshoot and return: the "click" with which the layer lands.
         *
         * `y1 > 1` is exactly the overshoot, and it is for its sake that `y` is not clamped.
         */
        val SNAP = EaseUi(Kind.CUBIC, 0.34f, 1.56f, 0.64f, 1f)

        /** A fast start and a sharp stop: for cuts and hits. */
        val HIT = EaseUi(Kind.CUBIC, 0.05f, 0.7f, 0.1f, 1f)

        /**
         * Exactly the ramp `3t² - 2t³` the editor used before curves.
         *
         * `cubic-bezier(1/3, 0, 2/3, 1)` has `x(u) = u` identically, so
         * its `y` is `3u² - 2u³` of the same argument — the same function, not a
         * similar one.
         */
        val SMOOTH = EaseUi(Kind.CUBIC, 1f / 3f, 0f, 2f / 3f, 1f)

        /**
         * Named presets — one list for the picker in the UI, the Rumi tool
         * schema and the tests: a preset that exists in the UI and not here would be
         * a name that is saved and never read.
         */
        val PRESETS: List<Pair<String, EaseUi>> = listOf(
            "linear" to EaseUi(),
            "hold" to EaseUi(Kind.HOLD),
            "ease" to EASE,
            "ease_in" to EASE_IN,
            "ease_out" to EASE_OUT,
            "ease_in_out" to EASE_IN_OUT,
            "smooth" to SMOOTH,
            "snap" to SNAP,
            "hit" to HIT,
        )

        /** A preset by name; `null` means there is no such name. */
        fun byName(name: String): EaseUi? = PRESETS.firstOrNull { it.first == name }?.second

        /**
         * Cubic Bézier at linear progress [t].
         *
         * The curve goes from `(0,0)` to `(1,1)` with control points `(x1,y1)` and
         * `(x2,y2)`. `x` is time, so the curve parameter `u` does not equal `t`:
         * first a `u` is sought such that `x(u) = t`, and the answer is `y(u)`.
         *
         * First Newton from `u = t` (it converges in three or four steps on any
         * curve a human draws), and if that fails — bisection: for a curve
         * with `x1 = x2 = 0` the derivative at the start is zero, and Newton diverges there.
         * Bisection is slower and always converges, so the fallback path is bounded by
         * an iteration count, not by hope.
         *
         * `x1`/`x2` are clamped to `0..=1`: outside that range `x(u)` stops
         * being monotonic, and the curve stops being a function of time — there is simply
         * no answer. `y1`/`y2` are left alone: overshoot is what is asked for
         * by name.
         */
        fun cubicBezier(t: Float, x1: Float, y1: Float, x2: Float, y2: Float): Float {
            // A non-finite control point is a corrupted document, not a shape:
            // it reads as a straight line so the frame stays finite.
            if (!(x1.isFinite() && y1.isFinite() && x2.isFinite() && y2.isFinite())) {
                return t.coerceIn(0f, 1f)
            }
            if (t <= 0f) return 0f
            if (t >= 1f) return 1f
            val cx = 3f * x1.coerceIn(0f, 1f)
            val bx = 3f * (x2.coerceIn(0f, 1f) - x1.coerceIn(0f, 1f)) - cx
            val ax = 1f - cx - bx
            val cy = 3f * y1
            val by = 3f * (y2 - y1) - cy
            val ay = 1f - cy - by

            fun sampleX(u: Float) = ((ax * u + bx) * u + cx) * u
            fun sampleDx(u: Float) = (3f * ax * u + 2f * bx) * u + cx
            fun sampleY(u: Float) = ((ay * u + by) * u + cy) * u

            var u = t
            for (i in 0 until 8) {
                val error = sampleX(u) - t
                if (kotlin.math.abs(error) < EPSILON) return sampleY(u)
                val slope = sampleDx(u)
                if (kotlin.math.abs(slope) < EPSILON) break
                u -= error / slope
            }

            var low = 0f
            var high = 1f
            u = t
            for (i in 0 until 64) {
                val x = sampleX(u)
                if (kotlin.math.abs(x - t) < EPSILON) break
                if (x < t) low = u else high = u
                u = (low + high) * 0.5f
            }
            return sampleY(u)
        }

        /**
         * How close `x(u)` must come to `t`.
         *
         * `1e-6` in the parameter is less than one pixel of motion for any
         * realistic layer size, that is, finer than a frame can show.
         */
        private const val EPSILON = 1e-6f
    }
}
