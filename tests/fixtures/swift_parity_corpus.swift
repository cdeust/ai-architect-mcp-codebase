import Foundation
import UIKit
import SwiftUI

let maxRetries = 3
var counter = 0

public func greet(name: String) -> String {
    return format(name)
}

private func helper() {
    compute()
}

struct Point {
    let x: Int
    let y: Int
    var label: String

    init(x: Int, y: Int) {
        self.x = x
        self.y = y
        self.label = describe()
    }

    func distance() -> Double {
        return compute(x, y)
    }

    var magnitude: Double {
        return sqrt(x)
    }

    subscript(i: Int) -> Int {
        return at(i)
    }
}

public class Animal {
    var name: String

    init(name: String) {
        self.name = name
    }

    deinit {
        cleanup()
    }

    open func speak() -> String {
        return sound()
    }
}

actor Counter {
    var value: Int = 0

    func increment() {
        bump()
    }
}

enum Color {
    case red
    case green, blue
    case custom(Int)

    func hex() -> String {
        return convert()
    }
}

protocol Serializable {
    func serialize() -> String
}

extension Animal {
    func describe() -> String {
        return render(name)
    }
}

typealias Handler = () -> Void

fileprivate let sharedFlag = true

func useClosures() {
    items.map { transform($0) }
    fetch { result in
        handle(result)
    }
    Utils.parse()
    self.reset()
}
