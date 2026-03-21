export function formatOutput(s: string): string {
    return s.trim().toLowerCase();
}

export class Formatter {
    private prefix: string;

    constructor(prefix: string) {
        this.prefix = prefix;
    }

    format(s: string): string {
        return `${this.prefix}${formatOutput(s)}`;
    }
}
