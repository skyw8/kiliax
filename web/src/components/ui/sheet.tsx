import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "../../lib/utils";

const Sheet = DialogPrimitive.Root;
const SheetTrigger = DialogPrimitive.Trigger;
const SheetPortal = DialogPrimitive.Portal;
const SheetClose = DialogPrimitive.Close;

const SheetOverlay = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Overlay>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Overlay>
>(({ className, ...props }, ref) => (
  <DialogPrimitive.Overlay
    ref={ref}
    className={cn(
      "fixed inset-0 z-50 bg-black/30 backdrop-blur-[1px] opacity-0 transition-opacity duration-200 data-[state=open]:opacity-100 data-[state=closed]:opacity-0",
      className,
    )}
    {...props}
  />
));
SheetOverlay.displayName = DialogPrimitive.Overlay.displayName;

const sheetContentVariants = cva(
  "fixed z-50 overflow-y-auto overscroll-contain border-zinc-200 bg-white shadow-lg outline-none transition-transform duration-200 will-change-transform",
  {
    variants: {
      side: {
        left: "inset-y-0 left-0 w-[min(320px,85vw)] border-r p-0 -translate-x-full data-[state=open]:translate-x-0 data-[state=closed]:-translate-x-full",
        bottom:
          "inset-x-0 bottom-0 max-h-[85dvh] rounded-t-xl border-t p-0 translate-y-full data-[state=open]:translate-y-0 data-[state=closed]:translate-y-full",
      },
    },
    defaultVariants: {
      side: "left",
    },
  },
);

const SheetContent = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Content> &
    VariantProps<typeof sheetContentVariants> & { showClose?: boolean }
>(({ className, children, side, showClose = true, ...props }, ref) => (
  <SheetPortal>
    <SheetOverlay />
    <DialogPrimitive.Content
      ref={ref}
      className={cn(sheetContentVariants({ side }), className)}
      {...props}
    >
      {children}
      {showClose ? (
        <SheetClose className="absolute right-3 top-3 rounded-md p-1 text-zinc-500 hover:bg-zinc-100">
          <X className="h-4 w-4" />
          <span className="sr-only">Close</span>
        </SheetClose>
      ) : null}
    </DialogPrimitive.Content>
  </SheetPortal>
));
SheetContent.displayName = DialogPrimitive.Content.displayName;

function SheetHeader({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={cn("mb-3 flex flex-col gap-1 px-4 pt-4", className)} {...props} />
  );
}

function SheetTitle({
  className,
  ...props
}: React.ComponentPropsWithoutRef<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      className={cn("text-sm font-semibold text-zinc-900", className)}
      {...props}
    />
  );
}

function SheetDescription({
  className,
  ...props
}: React.ComponentPropsWithoutRef<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      className={cn("text-sm text-zinc-600", className)}
      {...props}
    />
  );
}

export {
  Sheet,
  SheetTrigger,
  SheetPortal,
  SheetOverlay,
  SheetContent,
  SheetClose,
  SheetHeader,
  SheetTitle,
  SheetDescription,
};

