import { useCallback, useLayoutEffect, useRef } from "react";
import type { RefObject } from "react";

const phone = "(max-width: 760px)";
const scrollThreshold = 24;

export function useDiscussionPhoneLayout(
  ready: boolean,
  frameRef: RefObject<HTMLDivElement | null>,
  barRef: RefObject<HTMLElement | null>,
  timelineRef: RefObject<HTMLElement | null>,
) {
  const restore = useRef<(() => void) | null>(null);
  const resetScroll = useRef<(() => void) | null>(null);
  const revealHeader = useCallback(() => restore.current?.(), []);
  const beforeTimelinePosition = useCallback(() => resetScroll.current?.(), []);

  useLayoutEffect(() => {
    if (!ready || !frameRef.current || !barRef.current || !timelineRef.current)
      return;
    const frame = frameRef.current;
    const bar = barRef.current;
    const timeline = timelineRef.current;

    const media = window.matchMedia(phone);
    const visual = window.visualViewport;
    let collapsed = false;
    let keyboard = false;
    let resting:
      { width: number; height: number; layoutHeight: number } | undefined;
    const measuredRestingHeights = new Map<number, number>();
    let scheduled = 0;
    let lastGesture = -Infinity;
    let travel = 0;
    let lastHash = window.location.hash;
    let lastTop = timeline.scrollTop;
    let lastHeight = timeline.clientHeight;
    const atEnd = () =>
      timeline.scrollHeight - timeline.clientHeight - timeline.scrollTop <= 4;
    let atLatest = atEnd();

    function recordLayout() {
      lastTop = timeline.scrollTop;
      lastHeight = timeline.clientHeight;
      atLatest = atEnd();
    }

    function onTimelineResize() {
      if (!media.matches) return;
      // On a short thread there may be no scroll gesture available to bring
      // the toolbar back after the window or content changes size.
      if (
        collapsed &&
        !keyboard &&
        timeline.scrollHeight <= timeline.clientHeight + 1
      ) {
        setCollapsed(false);
        return;
      }
      if (timeline.clientHeight === lastHeight) return;
      // A reader stays on the same text. A reader already at the end keeps
      // seeing the newest message when the composer grows or the viewport shrinks.
      if (atLatest) timeline.scrollTop = timeline.scrollHeight;
      travel = 0;
      recordLayout();
    }

    function setCollapsed(next: boolean) {
      if (next === collapsed) return;
      const top = timeline.scrollTop;
      const edge = timeline.getBoundingClientRect().top;
      collapsed = next;
      frame.toggleAttribute("data-discussion-collapsed", next);
      const shift = timeline.getBoundingClientRect().top - edge;
      // Keep the same text at the same screen coordinate. At the top, leave
      // the first message reachable instead of scrolling it under the bar.
      timeline.scrollTop = top <= 1 ? 0 : top + shift;
      travel = 0;
      recordLayout();
    }

    restore.current = () => {
      if (!collapsed || keyboard) return;
      setCollapsed(false);
      bar.querySelector("a")?.focus({ preventScroll: true });
    };

    function clearScrollIntent() {
      lastGesture = -Infinity;
      travel = 0;
    }

    resetScroll.current = clearScrollIntent;

    function gesture(event: Event) {
      if (
        event instanceof KeyboardEvent &&
        (event.target !== timeline ||
          ![
            "ArrowDown",
            "ArrowUp",
            "PageDown",
            "PageUp",
            "Home",
            "End",
            " ",
          ].includes(event.key))
      )
        return;
      lastGesture = performance.now();
    }

    function onScroll() {
      if (!media.matches) return;
      if (lastHash !== window.location.hash) {
        lastHash = window.location.hash;
        clearScrollIntent();
      }
      if (timeline.clientHeight !== lastHeight) {
        onTimelineResize();
        return;
      }
      const top = Math.max(0, timeline.scrollTop);
      const delta = top - lastTop;
      recordLayout();
      if (!delta) return;
      if (keyboard || performance.now() - lastGesture > 350) {
        travel = 0;
        return;
      }
      if (delta < 0 && collapsed && top <= 1) {
        setCollapsed(false);
        return;
      }
      const direction = collapsed ? -1 : 1;
      // Small reversals cancel progress instead of toggling the header.
      travel = Math.max(0, travel + delta * direction);
      if (travel < scrollThreshold || (!collapsed && top <= bar.offsetHeight))
        return;
      travel = 0;
      if (
        !collapsed &&
        (bar.dataset.pinned === "true" ||
          bar.matches(":focus-within") ||
          bar.querySelector(".discussion-handoff-history[open]"))
      )
        return;
      setCollapsed(!collapsed);
    }

    function setKeyboardLayout(next: boolean, height = 0, top = 0) {
      if (next) {
        frame.style.setProperty("--discussion-visible-height", `${height}px`);
        frame.style.setProperty("--discussion-visible-top", `${top}px`);
      } else {
        frame.style.removeProperty("--discussion-visible-height");
        frame.style.removeProperty("--discussion-visible-top");
      }
      frame.toggleAttribute("data-discussion-keyboard", next);
    }

    function rememberResting(
      width: number,
      height: number,
      layoutHeight: number,
    ) {
      resting = { width, height, layoutHeight };
      measuredRestingHeights.set(width, height);
    }

    function restingHeightFor(width: number, layoutHeight: number) {
      const measured = measuredRestingHeights.get(width);
      if (measured !== undefined) return measured;
      if (!resting) return 0;
      // A co-resizing host might report no unoccluded height in the new
      // orientation. Use the closest pre-keyboard axis to estimate the new
      // perpendicular span without requiring the CSS dimensions to swap exactly.
      const sameAxis =
        Math.abs(width - resting.width) <=
        Math.abs(width - resting.layoutHeight);
      const span = sameAxis ? resting.layoutHeight : resting.width;
      const chrome = Math.max(0, resting.layoutHeight - resting.height);
      return Math.max(layoutHeight, span) - chrome;
    }

    function updateViewport() {
      scheduled = 0;
      const active = document.activeElement;
      const editing =
        !!active?.matches(".composer textarea") && frame.contains(active);
      if (!media.matches) {
        timeline.removeAttribute("tabindex");
        // Desktop still uses its normal layout. Keep a confirmed keyboard and
        // its resting measurement across a breakpoint while the editor keeps
        // focus, so an occluded sample on the way back cannot become the baseline.
        if (!editing) keyboard = false;
        setKeyboardLayout(false);
        setCollapsed(false);
        return;
      }
      timeline.tabIndex = 0;
      const height = visual?.height ?? window.innerHeight;
      const width = document.documentElement.clientWidth;
      const layoutHeight = document.documentElement.clientHeight;
      const scale = visual?.scale ?? 1;
      const unzoomed = Math.abs(scale - 1) < 0.02;
      if (unzoomed) {
        const previous = keyboard;
        if (previous) {
          const recovered = restingHeightFor(width, layoutHeight) - height < 72;
          keyboard = editing && !recovered;
          // Losing focus can precede viewport recovery. Don't store that last
          // occluded sample as a new resting height.
          if (recovered) rememberResting(width, height, layoutHeight);
        } else {
          if (
            !editing ||
            !resting ||
            width !== resting.width ||
            height > resting.height
          )
            rememberResting(width, height, layoutHeight);
          keyboard =
            editing && restingHeightFor(width, layoutHeight) - height >= 120;
        }
      } else if (!editing) {
        keyboard = false;
      }
      setKeyboardLayout(keyboard && unzoomed, height, visual?.offsetTop ?? 0);
      onTimelineResize();
    }

    function scheduleViewport() {
      if (!scheduled) scheduled = window.requestAnimationFrame(updateViewport);
    }

    const observer = new ResizeObserver(onTimelineResize);
    observer.observe(timeline);
    if (timeline.firstElementChild)
      observer.observe(timeline.firstElementChild);
    timeline.addEventListener("scroll", onScroll, { passive: true });
    timeline.addEventListener("wheel", gesture, { passive: true });
    timeline.addEventListener("touchmove", gesture, { passive: true });
    timeline.addEventListener("keydown", gesture);
    timeline.addEventListener("click", clearScrollIntent, true);
    frame.addEventListener("focusin", scheduleViewport);
    frame.addEventListener("focusout", scheduleViewport);
    visual?.addEventListener("resize", scheduleViewport);
    visual?.addEventListener("scroll", scheduleViewport);
    window.addEventListener("resize", scheduleViewport);
    window.addEventListener("pageshow", scheduleViewport);
    media.addEventListener("change", scheduleViewport);
    updateViewport();

    return () => {
      restore.current = null;
      resetScroll.current = null;
      window.cancelAnimationFrame(scheduled);
      observer.disconnect();
      timeline.removeEventListener("scroll", onScroll);
      timeline.removeEventListener("wheel", gesture);
      timeline.removeEventListener("touchmove", gesture);
      timeline.removeEventListener("keydown", gesture);
      timeline.removeEventListener("click", clearScrollIntent, true);
      frame.removeEventListener("focusin", scheduleViewport);
      frame.removeEventListener("focusout", scheduleViewport);
      visual?.removeEventListener("resize", scheduleViewport);
      visual?.removeEventListener("scroll", scheduleViewport);
      window.removeEventListener("resize", scheduleViewport);
      window.removeEventListener("pageshow", scheduleViewport);
      media.removeEventListener("change", scheduleViewport);
      setKeyboardLayout(false);
      frame.removeAttribute("data-discussion-collapsed");
      timeline.removeAttribute("tabindex");
    };
  }, [ready, frameRef, barRef, timelineRef]);

  return { revealHeader, beforeTimelinePosition };
}
