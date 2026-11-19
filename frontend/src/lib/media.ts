// One place for the wide-screen query: from 768 px a figure may animate; below it the plain value is shown.
import { useEffect, useState } from "react";

export function useWide(query = "(min-width: 768px)"): boolean {
  const [wide, setWide] = useState(() => matchMedia(query).matches);
  useEffect(() => {
    const mq = matchMedia(query);
    const on = () => setWide(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return wide;
}
