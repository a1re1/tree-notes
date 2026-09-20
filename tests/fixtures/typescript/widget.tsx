import { Props } from "./app";

export interface BadgeProps {
  label: string;
}

export function Badge(props: BadgeProps) {
  return <b>{props.label}</b>;
}

export const Pill = () => <i>pill</i>;
