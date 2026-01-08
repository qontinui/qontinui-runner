interface SectionHeaderProps {
  title: string;
  description: string;
  icon?: React.ReactNode;
}

export function SectionHeader({ title, description, icon }: SectionHeaderProps) {
  return (
    <div className="mb-4">
      <div className="flex items-center gap-2 mb-1">
        {icon && <span className="text-primary/70 [&>svg]:w-5 [&>svg]:h-5">{icon}</span>}
        <h3 className="text-base font-semibold">{title}</h3>
      </div>
      <p className="text-xs text-muted-foreground">{description}</p>
    </div>
  );
}
