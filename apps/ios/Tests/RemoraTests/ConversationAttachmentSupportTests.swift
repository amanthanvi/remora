import UIKit
import XCTest
@testable import Remora

final class ConversationAttachmentSupportTests: XCTestCase {
    func testUncertainDiscardRequiresMatchingRefreshCount() {
        XCTAssertFalse(isUncertainOutboxDiscardReady(currentCount: 1, refreshedCount: nil))
        XCTAssertFalse(isUncertainOutboxDiscardReady(currentCount: 2, refreshedCount: 1))
        XCTAssertFalse(isUncertainOutboxDiscardReady(currentCount: 0, refreshedCount: 0))
        XCTAssertTrue(isUncertainOutboxDiscardReady(currentCount: 2, refreshedCount: 2))
    }

    func testComposerPayloadRequiresLiveHostForNonTextContext() {
        let textOnly = AppComposerPayload(
            text: "queue me",
            additionalInputs: [],
            approvalPolicy: nil,
            sandboxPolicy: nil,
            model: nil,
            effort: nil,
            serviceTier: nil
        )
        let image = AppComposerPayload(
            text: "describe this",
            additionalInputs: [.image(url: "data:image/png;base64,abc")],
            approvalPolicy: nil,
            sandboxPolicy: nil,
            model: nil,
            effort: nil,
            serviceTier: nil
        )
        let file = AppComposerPayload(
            text: "review this",
            additionalInputs: [],
            fileAttachments: [ComposerFileAttachment(label: "main.swift", path: "/project/main.swift")],
            approvalPolicy: nil,
            sandboxPolicy: nil,
            model: nil,
            effort: nil,
            serviceTier: nil
        )

        XCTAssertEqual(textOnly.submissionContent, .textOnly)
        XCTAssertEqual(image.submissionContent, .liveHostRequired)
        XCTAssertEqual(file.submissionContent, .liveHostRequired)
    }

    func testBuildTurnInputsOmitsWhitespaceOnlyTextAndKeepsAttachmentInput() {
        let attachment = AppUserInput.image(url: "data:image/png;base64,abc")

        let inputs = ConversationAttachmentSupport.buildTurnInputs(
            text: "   \n",
            additionalInput: [attachment]
        )

        XCTAssertEqual(inputs.count, 1)
        guard case .image(let url)? = inputs.first else {
            return XCTFail("Expected image input")
        }
        XCTAssertEqual(url, "data:image/png;base64,abc")
    }

    func testPreparedAttachmentCreatesImageUserInput() throws {
        let attachment = try XCTUnwrap(
            PreparedImageAttachment(
                data: Data([0x01, 0x02, 0x03]),
                mimeType: "image/png"
            ) as PreparedImageAttachment?
        )

        guard case .image(let url) = attachment.userInput else {
            return XCTFail("Expected image user input")
        }

        XCTAssertEqual(url, "data:image/png;base64,AQID")
    }

    func testPrepareImageUsesPNGWhenImageHasTransparency() {
        let image = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).image { context in
            UIColor.clear.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
            UIColor.systemGreen.setFill()
            context.fill(CGRect(x: 1, y: 1, width: 2, height: 2))
        }

        let attachment = ConversationAttachmentSupport.prepareImage(image)

        XCTAssertEqual(attachment?.mimeType, "image/png")
        XCTAssertNotNil(attachment?.data)
    }

    func testPrepareImageUsesJPEGWhenImageIsOpaque() {
        let format = UIGraphicsImageRendererFormat()
        format.opaque = true
        let image = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4), format: format).image { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }

        let attachment = ConversationAttachmentSupport.prepareImage(image)

        XCTAssertEqual(attachment?.mimeType, "image/jpeg")
        XCTAssertNotNil(attachment?.data)
    }
}
