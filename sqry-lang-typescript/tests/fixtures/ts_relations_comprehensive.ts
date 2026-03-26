// Comprehensive TypeScript relations test fixture
// Tests all type-only features, type assertions, and generic handling

// ============================================================================
// IMPORTS - Type-only and mixed
// ============================================================================

// Type-only default import
import type User from './types/user';

// Type-only namespace import
import type * as Types from './types/index';

// Type-only named imports
import type { Config, Settings } from './types/config';

// Mixed type and value imports
import { type Handler, type Processor, getValue, processData } from './module';

// Regular imports (for comparison)
import React from 'react';
import { useState, useEffect } from 'react';
import * as Utils from './utils';

// ============================================================================
// EXPORTS - Type-only and mixed
// ============================================================================

// Interface exports (automatically type-only)
export interface UserProfile {
    id: string;
    name: string;
}

// Type alias exports (automatically type-only)
export type Callback = (data: string) => void;
export type AsyncCallback = (data: string) => Promise<void>;

// Enum exports (value export)
export enum Status {
    Active,
    Inactive,
    Pending
}

// Type-only export clause
export type { Config };

// Mixed type/value export clause
export { type Settings, getValue };

// Type-only re-export
export type { UserProfile as Profile } from './types/user';

// Mixed re-export
export { type Handler, processData } from './module';

// ============================================================================
// FUNCTIONS - Type assertions and generics
// ============================================================================

// Function with type assertion calls
function processUnknown(obj: unknown): void {
    // as_expression type assertion
    (obj as Handler).handle();

    // type_assertion (legacy syntax)
    (<Processor>obj).process();

    // Chained method with type assertion
    (obj as UserProfile).name.toLowerCase();
}

// Generic function calls
function transform<T>(data: T): T {
    // Generic call (types should be ignored at runtime)
    validate<T>(data);

    // Generic with multiple type parameters
    convert<T, string>(data);

    // Generic constructor (intentionally unused - testing parser detection)
    const _container = new Container<T>(data); // NOSONAR

    return data;
}

// Nested type assertions
function complexTypeAssertions(value: any): void {
    // Nested as expressions
    ((value as unknown) as Handler).handle();

    // Type assertion with member expression
    (value as { process: () => void }).process();
}

// Optional chaining with generics
function optionalChainGeneric<T>(obj: { method?: <U>(x: U) => void }): void {
    obj.method?.<string>('test');
}

// ============================================================================
// CLASSES - Methods with type assertions
// ============================================================================

class DataProcessor {
    process<T>(data: T): void {
        // Generic method call
        this.validate<T>(data);

        // Type assertion in method
        (data as any).transform();
    }

    validate<T>(data: T): boolean {
        return true;
    }
}

// ============================================================================
// ARROW FUNCTIONS - With type parameters
// ============================================================================

const mapper = <T, U>(fn: (x: T) => U) => (value: T): U => {
    // Arrow function with generics
    return fn(value);
};

const handler: Handler = (data: string) => {
    console.log(data);
};

// ============================================================================
// NAMESPACE - Exports and calls
// ============================================================================

namespace App {
    export function init(): void {
        connect();
        Database.query();
    }

    function connect(): void {
        console.log('connected');
    }

    export namespace Database {
        export function query(): void {
            console.log('querying');
        }
    }
}

// ============================================================================
// MODULE AUGMENTATION - Type-only exports
// ============================================================================

declare module './external' {
    export interface Extended {
        newMethod(): void;
    }

    export type ExtendedConfig = Config & { extra: boolean };
}

// ============================================================================
// HELPER FUNCTIONS - Referenced in calls above
// ============================================================================

function validate<T>(data: T): boolean {
    return true;
}

function convert<T, U>(data: T): U {
    return data as unknown as U;
}

class Container<T> {
    constructor(private data: T) {}
}

// ============================================================================
// EXPECTED EDGES SUMMARY
// ============================================================================

// IMPORTS (13 edges):
// 1. type User (default, type-only)
// 2. type * as Types (namespace, type-only, wildcard)
// 3. type Config (named, type-only)
// 4. type Settings (named, type-only)
// 5. type Handler (named, type-only)
// 6. type Processor (named, type-only)
// 7. getValue (named, value)
// 8. processData (named, value)
// 9. React (default, value)
// 10. useState (named, value)
// 11. useEffect (named, value)
// 12. * as Utils (namespace, value, wildcard)

// EXPORTS (12 edges):
// 1. UserProfile (interface, type-only)
// 2. Callback (type alias, type-only)
// 3. AsyncCallback (type alias, type-only)
// 4. Status (enum, value)
// 5. Config (type-only re-export)
// 6. Settings (type-only, from clause)
// 7. getValue (value, from clause)
// 8. Profile (type-only, aliased re-export)
// 9. Handler (type-only re-export)
// 10. processData (value re-export)
// 11. App.init (namespace function)
// 12. App.Database.query (nested namespace function)

// CALLS (15+ edges):
// From processUnknown: handle, process, toLowerCase
// From transform: validate, convert, Container
// From complexTypeAssertions: handle, process
// From optionalChainGeneric: method
// From DataProcessor.process: validate, transform
// From App.init: connect, query
