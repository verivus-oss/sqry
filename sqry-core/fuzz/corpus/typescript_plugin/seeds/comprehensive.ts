// Comprehensive TypeScript relations test fixture
// Tests all type-only features, type assertions, and generic handling

// Type-only imports
import type User from './types/user';
import type * as Types from './types/index';
import type { Config, Settings } from './types/config';

// Mixed type and value imports
import { type Handler, type Processor, getValue, processData } from './module';

// Regular imports
import React from 'react';
import { useState, useEffect } from 'react';

// Interface exports
export interface UserProfile {
    id: string;
    name: string;
}

// Type alias exports
export type Callback = (data: string) => void;
export type AsyncCallback = (data: string) => Promise<void>;

// Enum exports
export enum Status {
    Active,
    Inactive,
    Pending
}

// Classes with generics
export class DataProcessor<T> {
    private data: T[];

    constructor(initial: T[]) {
        this.data = initial;
    }

    process(item: T): Promise<T> {
        return Promise.resolve(item);
    }
}

// Functions with type parameters
export function identity<T>(arg: T): T {
    return arg;
}

// Optional chaining and type assertions
function processUser(user?: UserProfile) {
    const name = user?.name as string;
    return name;
}
