/**
 * Webpack configuration for sqry VS Code extension
 * Bundles TypeScript source into optimized JavaScript for faster activation
 */

//@ts-check
'use strict';

const path = require('node:path');

/**@type {import('webpack').Configuration}*/
const config = {
  target: 'node', // VS Code extensions run in Node.js context
  mode: 'none', // Leave source code as close as possible (no minification for debugging)

  entry: './src/extension.ts', // Extension entry point
  output: {
    path: path.resolve(__dirname, 'dist'),
    filename: 'extension.js',
    libraryTarget: 'commonjs2',
    devtoolModuleFilenameTemplate: '../[resource-path]'
  },
  devtool: 'source-map',
  externals: {
    vscode: 'commonjs vscode' // Don't bundle vscode module (provided by VS Code)
  },
  resolve: {
    extensions: ['.ts', '.js']
  },
  module: {
    rules: [
      {
        test: /\.ts$/,
        exclude: /node_modules/,
        use: [
          {
            loader: 'ts-loader'
          }
        ]
      }
    ]
  }
};

module.exports = config;
