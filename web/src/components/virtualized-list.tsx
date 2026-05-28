import React from "react";

export type ListRange = {
  startIndex: number;
  endIndex: number;
};

export type VirtuosoHandle = {
  scrollToIndex: (location: {
    index: number | "LAST";
    align?: "start" | "center" | "end";
    behavior?: ScrollBehavior;
  }) => void;
};

type VirtuosoProps<T> = {
  customScrollParent: HTMLElement;
  data: T[];
  firstItemIndex: number;
  followOutput?: (atBottom: boolean) => false | ScrollBehavior;
  increaseViewportBy?: { top?: number; bottom?: number };
  startReached?: () => void;
  rangeChanged?: (range: ListRange) => void;
  computeItemKey?: (index: number, item: T) => React.Key;
  components?: {
    Header?: React.ComponentType;
  };
  itemContent: (index: number, item: T) => React.ReactNode;
};

const DEFAULT_ITEM_HEIGHT = 96;
const START_REACHED_PX = 160;
const BOTTOM_EPSILON_PX = 2;
const USER_SCROLL_EPSILON_PX = 1;

function itemKey<T>(
  props: VirtuosoProps<T>,
  itemIndex: number,
  item: T,
): string {
  return String(
    props.computeItemKey?.(props.firstItemIndex + itemIndex, item) ??
      props.firstItemIndex + itemIndex,
  );
}

function VirtualizedRow<T>({
  item,
  itemIndex,
  top,
  props,
  onSize,
}: {
  item: T;
  itemIndex: number;
  top: number;
  props: VirtuosoProps<T>;
  onSize: (key: string, size: number) => void;
}) {
  const ref = React.useRef<HTMLDivElement | null>(null);
  const key = itemKey(props, itemIndex, item);

  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => onSize(key, el.getBoundingClientRect().height);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [key, onSize]);

  return (
    <div
      ref={ref}
      data-virtual-key={key}
      className="absolute inset-x-0"
      style={{ transform: `translateY(${top}px)` }}
    >
      {props.itemContent(props.firstItemIndex + itemIndex, item)}
    </div>
  );
}

function HeaderMeasure({
  Header,
  onSize,
}: {
  Header?: React.ComponentType;
  onSize: (size: number) => void;
}) {
  const ref = React.useRef<HTMLDivElement | null>(null);

  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => onSize(el.getBoundingClientRect().height);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [onSize]);

  return <div ref={ref}>{Header ? <Header /> : null}</div>;
}

function VirtuosoInner<T>(
  props: VirtuosoProps<T>,
  ref: React.Ref<VirtuosoHandle>,
) {
  const rootRef = React.useRef<HTMLDivElement | null>(null);
  const propsRef = React.useRef(props);
  const [headerHeight, setHeaderHeight] = React.useState(0);
  const [sizes, setSizes] = React.useState<Record<string, number>>({});
  const [viewport, setViewport] = React.useState({
    scrollTop: 0,
    viewportHeight: props.customScrollParent.clientHeight,
    rootTop: 0,
  });
  const previousDataLengthRef = React.useRef(props.data.length);
  const stickToBottomRef = React.useRef(true);
  const lastScrollTopRef = React.useRef(props.customScrollParent.scrollTop);
  const followFrameRef = React.useRef<number | null>(null);

  propsRef.current = props;

  const measureViewport = React.useCallback(() => {
    const root = rootRef.current;
    const parent = propsRef.current.customScrollParent;
    if (!root || !parent) return;
    const parentRect = parent.getBoundingClientRect();
    const rootRect = root.getBoundingClientRect();
    const distance = parent.scrollHeight - parent.scrollTop - parent.clientHeight;
    if (parent.scrollTop < lastScrollTopRef.current - USER_SCROLL_EPSILON_PX) {
      stickToBottomRef.current = false;
      if (followFrameRef.current != null) {
        cancelAnimationFrame(followFrameRef.current);
        followFrameRef.current = null;
      }
    } else if (distance <= BOTTOM_EPSILON_PX) {
      stickToBottomRef.current = true;
    }
    lastScrollTopRef.current = parent.scrollTop;
    setViewport({
      scrollTop: parent.scrollTop,
      viewportHeight: parent.clientHeight,
      rootTop: parent.scrollTop + rootRect.top - parentRect.top,
    });
  }, []);

  React.useLayoutEffect(() => {
    lastScrollTopRef.current = props.customScrollParent.scrollTop;
    measureViewport();
    const parent = props.customScrollParent;
    parent.addEventListener("scroll", measureViewport, { passive: true });
    window.addEventListener("resize", measureViewport);
    const observer = new ResizeObserver(measureViewport);
    observer.observe(parent);
    return () => {
      parent.removeEventListener("scroll", measureViewport);
      window.removeEventListener("resize", measureViewport);
      observer.disconnect();
    };
  }, [measureViewport, props.customScrollParent]);

  React.useEffect(() => {
    return () => {
      if (followFrameRef.current != null) {
        cancelAnimationFrame(followFrameRef.current);
        followFrameRef.current = null;
      }
    };
  }, []);

  const offsets = React.useMemo(() => {
    let total = 0;
    return props.data.map((item, idx) => {
      const top = total;
      const key = itemKey(props, idx, item);
      total += sizes[key] ?? DEFAULT_ITEM_HEIGHT;
      return {
        key,
        top,
        height: sizes[key] ?? DEFAULT_ITEM_HEIGHT,
      };
    });
  }, [props, sizes]);

  const totalHeight = offsets.length
    ? offsets[offsets.length - 1]!.top + offsets[offsets.length - 1]!.height
    : 0;
  const virtualTop = Math.max(0, viewport.scrollTop - viewport.rootTop - headerHeight);
  const overscanTop = props.increaseViewportBy?.top ?? 0;
  const overscanBottom = props.increaseViewportBy?.bottom ?? 0;
  const visibleTop = Math.max(0, virtualTop - overscanTop);
  const visibleBottom = virtualTop + viewport.viewportHeight + overscanBottom;

  let start = 0;
  while (
    start < offsets.length &&
    offsets[start]!.top + offsets[start]!.height < visibleTop
  ) {
    start += 1;
  }

  let end = start;
  while (end < offsets.length && offsets[end]!.top <= visibleBottom) {
    end += 1;
  }
  end = Math.min(offsets.length - 1, Math.max(start, end));

  React.useEffect(() => {
    if (!props.rangeChanged || !props.data.length) return;
    props.rangeChanged({
      startIndex: props.firstItemIndex + start,
      endIndex: props.firstItemIndex + end,
    });
  }, [end, props, start]);

  React.useEffect(() => {
    if (!props.startReached) return;
    if (viewport.scrollTop - viewport.rootTop <= START_REACHED_PX) {
      props.startReached();
    }
  }, [props, viewport.rootTop, viewport.scrollTop]);

  const scrollToDataIndex = React.useCallback(
    (
      dataIndex: number,
      align: "start" | "center" | "end" = "start",
      behavior: ScrollBehavior = "auto",
    ) => {
      const parent = propsRef.current.customScrollParent;
      const offset = offsets[dataIndex];
      if (!parent || !offset) return;
      if (align === "end" && dataIndex >= propsRef.current.data.length - 1) {
        stickToBottomRef.current = true;
      }
      let top = viewport.rootTop + headerHeight + offset.top;
      if (align === "center") {
        top -= Math.max(0, (parent.clientHeight - offset.height) / 2);
      } else if (align === "end") {
        top -= Math.max(0, parent.clientHeight - offset.height);
      }
      parent.scrollTo({ top: Math.max(0, top), behavior });
    },
    [headerHeight, offsets, viewport.rootTop],
  );

  const scrollToBottom = React.useCallback(
    (behavior: ScrollBehavior = "auto") => {
      const parent = propsRef.current.customScrollParent;
      stickToBottomRef.current = true;
      if (followFrameRef.current != null) {
        return;
      }
      followFrameRef.current = requestAnimationFrame(() => {
        followFrameRef.current = null;
        parent.scrollTo({ top: parent.scrollHeight, behavior });
      });
    },
    [],
  );

  React.useImperativeHandle(
    ref,
    () => ({
      scrollToIndex(location) {
        const dataIndex =
          location.index === "LAST"
            ? propsRef.current.data.length - 1
            : Math.max(0, Number(location.index) - propsRef.current.firstItemIndex);
        scrollToDataIndex(dataIndex, location.align, location.behavior);
      },
    }),
    [scrollToDataIndex],
  );

  React.useLayoutEffect(() => {
    const prev = previousDataLengthRef.current;
    previousDataLengthRef.current = props.data.length;
    if (props.data.length <= prev || !props.followOutput) return;
    const behavior = props.followOutput(stickToBottomRef.current);
    if (!behavior) return;
    requestAnimationFrame(() => {
      scrollToDataIndex(props.data.length - 1, "end", behavior);
    });
  }, [props, scrollToDataIndex]);

  const handleSize = React.useCallback((key: string, size: number) => {
    const shouldFollow = propsRef.current.followOutput && stickToBottomRef.current;
    setSizes((prev) => {
      if (Math.abs((prev[key] ?? 0) - size) < 0.5) return prev;
      return { ...prev, [key]: size };
    });
    if (shouldFollow) {
      scrollToBottom("auto");
    }
  }, [scrollToBottom]);

  const Header = props.components?.Header;

  return (
    <div ref={rootRef} className="relative min-h-full">
      <HeaderMeasure Header={Header} onSize={setHeaderHeight} />
      <div className="relative" style={{ height: totalHeight }}>
        {props.data.slice(start, end + 1).map((item, idx) => {
          const itemIndex = start + idx;
          return (
            <VirtualizedRow
              key={offsets[itemIndex]?.key ?? itemIndex}
              item={item}
              itemIndex={itemIndex}
              top={offsets[itemIndex]?.top ?? 0}
              props={props}
              onSize={handleSize}
            />
          );
        })}
      </div>
    </div>
  );
}

export const Virtuoso = React.forwardRef(VirtuosoInner) as <T>(
  props: VirtuosoProps<T> & { ref?: React.Ref<VirtuosoHandle> },
) => React.ReactElement | null;
