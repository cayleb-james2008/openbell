// Lean, zero-bloat UI primitives for OpenBell Desktop Quant Workbench.
// Replaces heavy @base-ui/react, @number-flow/react, react-use-measure, and recharts wrappers.
import { type ComponentPropsWithRef, type HTMLAttributes, type ReactNode, useState } from "react";
import { ChevronDown } from "lucide-react";
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export type ButtonVariant = "default" | "outline" | "ghost" | "danger";
export type ButtonSize = "default" | "sm" | "icon";

export function Button({
  variant = "outline",
  size = "default",
  className,
  type = "button",
  ...props
}: ComponentPropsWithRef<"button"> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
}) {
  return (
    <button
      type={type}
      className={cn(
        "inline-flex cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-md border font-medium transition-[background-color,color,border-color] duration-(--dur-press) ease-(--ease-house) outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:bg-track disabled:text-ink-3 disabled:border-hairline",
        size === "default" && "min-h-11 px-3.5 text-sm sm:min-h-9",
        size === "sm" && "min-h-11 px-2.5 text-sm sm:min-h-8 sm:text-xs",
        size === "icon" && "size-11 shrink-0 sm:size-9",
        variant === "default" &&
          "border-blue bg-blue text-blue-fg shadow-(--elev-primary) hover:border-blue-hover hover:bg-blue-hover",
        variant === "outline" &&
          "border-hairline bg-surface text-ink hover:bg-board",
        variant === "ghost" &&
          "border-transparent bg-transparent text-ink-2 hover:bg-board hover:text-ink",
        variant === "danger" &&
          "border-dot-red bg-surface text-destructive-foreground hover:bg-board",
        className,
      )}
      {...props}
    />
  );
}

export function Badge({
  className,
  variant = "outline",
  ...props
}: HTMLAttributes<HTMLSpanElement> & { variant?: "outline" | "secondary" | "success" | "warning" | "error" }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium whitespace-nowrap",
        variant === "outline" && "border border-hairline bg-surface text-ink-2",
        variant === "secondary" && "bg-track text-ink-2",
        variant === "success" && "bg-pill-active-bg text-pill-active-fg",
        variant === "warning" && "bg-pill-warn-bg text-pill-warn-fg",
        variant === "error" && "border border-dot-red/50 bg-surface text-destructive-foreground",
        className,
      )}
      {...props}
    />
  );
}

export type StatusTone = "info" | "success" | "warning" | "error" | "neutral";

export function StatusPill({
  id,
  tone = "neutral",
  title,
  className,
  children,
}: {
  id?: string;
  tone?: StatusTone;
  title?: string;
  className?: string;
  children: ReactNode;
}) {
  const dotCls =
    tone === "success"
      ? "bg-dot-green"
      : tone === "warning"
        ? "bg-dot-amber"
        : tone === "error"
          ? "bg-dot-red"
          : tone === "info"
            ? "bg-dot-blue"
            : "bg-ink-3";
  return (
    <span
      id={id}
      title={title}
      className={cn(
        "inline-flex min-h-7 items-center gap-1.5 rounded-full border border-hairline bg-surface px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-ink-2",
        className,
      )}
    >
      <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", dotCls)} />
      {children}
    </span>
  );
}

export function Kbd({ className, children }: { className?: string; children: ReactNode }) {
  return (
    <kbd
      className={cn(
        "mono inline-flex min-w-5 items-center justify-center rounded border border-hairline bg-track px-1.5 py-0.5 text-xs font-medium text-ink-2",
        className,
      )}
    >
      {children}
    </kbd>
  );
}

export function Table({ className, ...props }: HTMLAttributes<HTMLTableElement>) {
  return (
    <div className="w-full overflow-x-auto">
      <table className={cn("w-full border-collapse text-left text-sm", className)} {...props} />
    </div>
  );
}

export function TableHeader({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return <thead className={cn("border-b border-hairline bg-board/60", className)} {...props} />;
}

export function TableBody({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return <tbody className={cn("divide-y divide-hairline", className)} {...props} />;
}

export function TableRow({ className, ...props }: HTMLAttributes<HTMLTableRowElement>) {
  return <tr className={cn("transition-[background-color] duration-100 hover:bg-board/50", className)} {...props} />;
}

export function TableHead({ className, ...props }: HTMLAttributes<HTMLTableCellElement>) {
  return (
    <th
      className={cn("px-3 py-2.5 text-xs font-medium text-ink-2 whitespace-nowrap", className)}
      {...props}
    />
  );
}

export function TableCell({ className, ...props }: HTMLAttributes<HTMLTableCellElement>) {
  return <td className={cn("px-3 py-2.5 align-middle text-sm text-ink", className)} {...props} />;
}

export type TrackerBlock = { color: string; tooltip?: string };

export function TrackerBar({ data, className }: { data: TrackerBlock[]; className?: string }) {
  return (
    <div className={cn("flex h-4 w-full items-center gap-0.5", className)} aria-label="Status sequence">
      {data.map((b, idx) => (
        <span
          key={idx}
          title={b.tooltip}
          className={cn("h-full flex-1 rounded-[2px]", b.color)}
        />
      ))}
    </div>
  );
}

export function Disclosure({
  label,
  defaultOpen = false,
  className,
  children,
}: {
  label: ReactNode;
  defaultOpen?: boolean;
  className?: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={cn("w-full", className)}>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-2 text-left text-sm font-medium text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
      >
        <span>{label}</span>
        <ChevronDown
          className={cn("size-4 shrink-0 text-ink-2 transition-transform duration-150", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open && <div className="pt-2">{children}</div>}
    </div>
  );
}
