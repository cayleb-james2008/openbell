// Motion props for the library parts, kept here so Tailwind does not read a JSX prop name as a class.
import { glide } from "@/motion";

export const sectionGlide = { transition: glide };
export const drawOnce = { transition: { duration: 0.35, ease: [0.23, 1, 0.32, 1] as [number, number, number, number] } };
