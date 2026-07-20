# Why: Groove, swing, and feel are separate Operators that re-time Message streams per-stream, not behavior buried in the Clock.

[Rule](../../signal-time-dsp.md#groove-is-separate-operators)

Groove, swing, and feel are *per-stream* qualities — a hi-hat can swing while the bass stays straight
— so baking them into the Clock would force one feel on everything the Clock times and make
differential grooves impossible. Keeping the Clock to base timing only ([clock-is-an-operator](clock-is-an-operator.md))
and expressing feel as **separate Operators that re-time Message streams** makes groove composable:
insert one on the stream you want to bend, leave the others alone. A default global groove is a fine
on-ramp, but it is an *operator you can remove*, not a property of the transport.

This is the same modular instinct that runs through the whole DSP surface — a contour generator is
separate from the thing that applies it ([envelope-emits-cv](envelope-emits-cv.md)), a curve is its
own op ([curve-ops-named-for-math](curve-ops-named-for-math.md)) — timing feel is one more thing you
patch, not a mode.

Distilled from: ADR-0006
